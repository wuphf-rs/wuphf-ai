package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"os"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	coreclient "github.com/akshettrj/wuphf/connectors/whatsapp/internal/core"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	_ "github.com/mattn/go-sqlite3"
	"github.com/mdp/qrterminal/v3"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types/events"
)

type Connector struct {
	cfg config.File
}

func New(cfg config.File) Connector {
	return Connector{cfg: cfg}
}

func (c Connector) Run(ctx context.Context) error {
	targets, err := NewTargetRegistry(c.cfg)
	if err != nil {
		return fmt.Errorf("configure WhatsApp targets: %w", err)
	}
	container, err := sqlstore.New(ctx, "sqlite3", c.cfg.SessionDatabase, nil)
	if err != nil {
		return fmt.Errorf("open WhatsApp session database: %w", err)
	}
	defer container.Close()
	device, err := container.GetFirstDevice(ctx)
	if err != nil {
		return fmt.Errorf("load WhatsApp device: %w", err)
	}
	client := whatsmeow.NewClient(device, nil)
	media, err := coreclient.DialMedia(c.cfg.CoreAddress)
	if err != nil {
		return err
	}
	defer media.Close()
	inputs := make(chan *wuphfpb.ClientFrame, 128)
	normalizer := NewNormalizer(targets, inputs, NewMediaCodec(media, client))
	eventErrors := make(chan error, 1)
	client.AddEventHandler(func(raw any) {
		message, ok := raw.(*events.Message)
		if !ok {
			return
		}
		if err := normalizer.Handle(ctx, message); err != nil {
			select {
			case eventErrors <- err:
			default:
			}
		}
	})
	if err := connect(ctx, client); err != nil {
		return err
	}
	defer client.Disconnect()
	if client.Store.ID == nil {
		return errors.New("WhatsApp session has no account JID after pairing")
	}
	ownJID := *client.Store.ID
	runResult := make(chan error, 1)
	go func() {
		runResult <- coreclient.New(c.cfg).Run(ctx, NewSender(client, targets, media, ownJID), inputs)
	}()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-eventErrors:
		return fmt.Errorf("normalize WhatsApp event: %w", err)
	case err := <-runResult:
		return err
	}
}

func connect(ctx context.Context, client *whatsmeow.Client) error {
	if client.Store.ID != nil {
		if err := client.Connect(); err != nil {
			return fmt.Errorf("connect WhatsApp session: %w", err)
		}
		return nil
	}
	codes, err := client.GetQRChannel(ctx)
	if err != nil {
		return fmt.Errorf("open WhatsApp QR pairing: %w", err)
	}
	if err := client.Connect(); err != nil {
		return fmt.Errorf("connect WhatsApp for pairing: %w", err)
	}
	for code := range codes {
		switch code.Event {
		case "code":
			fmt.Fprintln(os.Stderr, "Scan WhatsApp QR code:")
			qrterminal.GenerateHalfBlock(code.Code, qrterminal.L, os.Stderr)
		case "success":
			return nil
		default:
			return fmt.Errorf("WhatsApp QR pairing failed: %s", code.Event)
		}
	}
	return errors.New("WhatsApp QR pairing stopped before success")
}
