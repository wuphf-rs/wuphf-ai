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
	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types/events"
	waLog "go.mau.fi/whatsmeow/util/log"
)

type Connector struct {
	cfg    config.File
	logger zerolog.Logger
}

func New(cfg config.File, logger zerolog.Logger) Connector {
	return Connector{cfg: cfg, logger: logger}
}

func (c Connector) Run(ctx context.Context) error {
	dbLog := waLog.Zerolog(c.logger.With().Str("component", "whatsmeow-db").Logger())
	container, err := sqlstore.New(ctx, "sqlite3", c.cfg.SessionDatabase, dbLog)
	if err != nil {
		return fmt.Errorf("open WhatsApp session database: %w", err)
	}
	defer container.Close()
	// Register as an Android client (experimental) so WhatsApp delivers
	// view-once media to this companion; must be set before pairing, which
	// connect() may trigger below. A WhatsApp Business account pairs as the SMB
	// Android client instead.
	mode := c.cfg.ResolvedClientMode()
	c.logger.Warn().Str("client_mode", mode).Msg("registering WhatsApp session as an Android client (experimental)")
	useAndroidClientIdentity(mode == config.ClientModeAndroidBusiness)
	device, err := container.GetFirstDevice(ctx)
	if err != nil {
		return fmt.Errorf("load WhatsApp device: %w", err)
	}
	client := whatsmeow.NewClient(device, waLog.Zerolog(c.logger.With().Str("component", "whatsmeow").Logger()))
	media, err := coreclient.DialMedia(c.cfg.CoreAddress)
	if err != nil {
		return err
	}
	defer media.Close()
	inputs := make(chan *wuphfpb.ClientFrame, 128)
	normalizer := NewNormalizer(inputs, NewMediaCodec(media, client), newChatNamer(client), newIdentityResolver(client))
	eventErrors := make(chan error, 1)
	client.AddEventHandler(func(raw any) {
		var err error
		switch evt := raw.(type) {
		case *events.Message:
			err = normalizer.Handle(ctx, evt)
		case *events.Receipt:
			err = normalizer.HandleReceipt(ctx, evt)
		default:
			return
		}
		if err != nil {
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
	c.logger.Info().Str("jid", ownJID.String()).Msg("WhatsApp session connected")
	runResult := make(chan error, 1)
	go func() {
		runResult <- coreclient.New(c.cfg, c.logger).Run(ctx, NewSender(client, media, ownJID, c.cfg.StickerMetadata(), c.cfg.Rendering), inputs)
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
