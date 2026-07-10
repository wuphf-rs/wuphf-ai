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
	"github.com/rs/zerolog"
	"golang.org/x/term"
)

type Connector struct {
	cfg         config.File
	credentials config.Credentials
	logger      zerolog.Logger
}

func New(cfg config.File, credentials config.Credentials, logger zerolog.Logger) Connector {
	return Connector{cfg: cfg, credentials: credentials, logger: logger}
}

func (c Connector) Run(ctx context.Context) error {
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
	peers := NewPeerCache()
	inputs := make(chan *wuphfpb.ClientFrame, 128)
	dispatcher := NewNormalizer(peers, inputs, codec).Dispatcher()
	client = gotd.NewClient(c.credentials.AppID, c.credentials.AppHash, gotd.Options{
		SessionStorage: &session.FileStorage{Path: c.cfg.SessionFile},
		UpdateHandler:  dispatcher,
	})
	return client.Run(ctx, func(ctx context.Context) error {
		if err := c.authenticate(ctx, client); err != nil {
			return err
		}
		// Bots cannot list dialogs (messages.getDialogs is user-only), and they
		// receive an access hash on the first update from any peer, so cold-start
		// priming applies only to user sessions.
		if c.credentials.BotToken == "" {
			if err := primePeers(ctx, client.API(), peers); err != nil {
				return err
			}
		}
		c.logger.Info().Msg("Telegram session connected")
		sender := NewSender(client.API(), peers, media, c.cfg.Rendering)
		return coreclient.New(c.cfg, c.logger).Run(ctx, sender, inputs)
	})
}

// primePeers seeds the peer cache from the account's dialog list so the core can
// dispatch to a configured chat (e.g. a mirror container) before any update from
// it has been observed. MTProto requires an access hash to address a user or
// channel, and the dialog list is the cheapest source of hashes at startup.
func primePeers(ctx context.Context, api *tg.Client, peers *PeerCache) error {
	dialogs, err := api.MessagesGetDialogs(ctx, &tg.MessagesGetDialogsRequest{
		OffsetPeer: &tg.InputPeerEmpty{},
		Limit:      100,
	})
	if err != nil {
		return fmt.Errorf("prime Telegram peer cache: %w", err)
	}
	switch value := dialogs.(type) {
	case *tg.MessagesDialogs:
		peers.ObserveClasses(value.Users, value.Chats)
	case *tg.MessagesDialogsSlice:
		peers.ObserveClasses(value.Users, value.Chats)
	}
	return nil
}

func (c Connector) authenticate(ctx context.Context, client *gotd.Client) error {
	// Reuse the stored session when it is still authorized. Telegram heavily
	// rate-limits auth calls (FLOOD_WAIT), and bot ImportBotAuthorization is not
	// self-guarded, so re-authenticating on every restart eventually locks us out.
	if status, err := client.Auth().Status(ctx); err == nil && status.Authorized {
		return nil
	}

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

	authenticator := passwordPrompt{
		UserAuthenticator: auth.Constant(c.credentials.Phone, c.credentials.Password, codePrompt),
		configured:        c.credentials.Password,
	}
	if err := auth.NewFlow(authenticator, auth.SendCodeOptions{}).Run(ctx, client.Auth()); err != nil {
		return fmt.Errorf("authenticate Telegram user: %w", err)
	}
	return nil
}

// passwordPrompt answers the 2FA password challenge: it returns the configured
// password when one is set, and otherwise prompts for it interactively without
// echoing. Phone, code, sign-up, and ToS handling are inherited unchanged.
type passwordPrompt struct {
	auth.UserAuthenticator
	configured string
}

func (p passwordPrompt) Password(_ context.Context) (string, error) {
	if p.configured != "" {
		return p.configured, nil
	}
	fmt.Fprint(os.Stderr, "Telegram 2FA password: ")
	secret, err := term.ReadPassword(int(os.Stdin.Fd()))
	fmt.Fprintln(os.Stderr)
	if err != nil {
		return "", fmt.Errorf("read Telegram 2FA password: %w", err)
	}
	return strings.TrimSpace(string(secret)), nil
}
