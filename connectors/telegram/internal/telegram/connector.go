package telegramconnector

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	coreclient "github.com/akshettrj/wuphf/connectors/telegram/internal/core"
	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/session"
	gotd "github.com/gotd/td/telegram"
	"github.com/gotd/td/telegram/auth"
	"github.com/gotd/td/tg"
)

type Connector struct {
	cfg         config.File
	credentials config.Credentials
}

func New(cfg config.File, credentials config.Credentials) Connector {
	return Connector{cfg: cfg, credentials: credentials}
}

func (c Connector) Run(ctx context.Context) error {
	targets, err := NewTargetRegistry(c.cfg)
	if err != nil {
		return fmt.Errorf("configure Telegram targets: %w", err)
	}
	media, err := coreclient.DialMedia(c.cfg.CoreAddress)
	if err != nil {
		return err
	}
	defer media.Close()
	var client *gotd.Client
	codec := NewMediaCodec(media, func(ctx context.Context, location tg.InputFileLocationClass) ([]byte, error) {
		var output bytes.Buffer
		if _, err := client.Download(location).Stream(ctx, &output); err != nil {
			return nil, err
		}
		return output.Bytes(), nil
	})
	inputs := make(chan *wuphfpb.ClientFrame, 128)
	dispatcher := NewNormalizer(targets, inputs, codec).Dispatcher()
	client = gotd.NewClient(c.credentials.AppID, c.credentials.AppHash, gotd.Options{
		SessionStorage: &session.FileStorage{Path: c.cfg.SessionFile},
		UpdateHandler:  dispatcher,
	})
	return client.Run(ctx, func(ctx context.Context) error {
		if err := c.authenticate(ctx, client); err != nil {
			return err
		}
		sender := NewSender(client.API(), targets, media)
		return coreclient.New(c.cfg).Run(ctx, sender, inputs)
	})
}

func (c Connector) authenticate(ctx context.Context, client *gotd.Client) error {
	if c.credentials.BotToken != "" {
		if _, err := client.Auth().Bot(ctx, c.credentials.BotToken); err != nil {
			return fmt.Errorf("authenticate Telegram bot: %w", err)
		}
		return nil
	}

	codePrompt := auth.CodeAuthenticatorFunc(func(_ context.Context, _ *tg.AuthSentCode) (string, error) {
		fmt.Fprint(os.Stderr, "Telegram authentication code: ")
		code, err := bufio.NewReader(os.Stdin).ReadString('\n')
		if err != nil {
			return "", err
		}
		return strings.TrimSpace(code), nil
	})

	var authenticator auth.UserAuthenticator
	if c.credentials.Password == "" {
		authenticator = auth.CodeOnly(c.credentials.Phone, codePrompt)
	} else {
		authenticator = auth.Constant(c.credentials.Phone, c.credentials.Password, codePrompt)
	}
	if err := auth.NewFlow(authenticator, auth.SendCodeOptions{}).Run(ctx, client.Auth()); err != nil {
		return fmt.Errorf("authenticate Telegram user: %w", err)
	}
	return nil
}
