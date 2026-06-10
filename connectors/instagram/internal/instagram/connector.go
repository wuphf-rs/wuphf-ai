package instagramconnector

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"

	"github.com/akshettrj/wuphf/connectors/instagram/internal/config"
	coreclient "github.com/akshettrj/wuphf/connectors/instagram/internal/core"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"
)

type Connector struct {
	cfg    config.File
	logger zerolog.Logger
}

func New(cfg config.File, logger zerolog.Logger) Connector {
	return Connector{cfg: cfg, logger: logger}
}

func (c Connector) Run(ctx context.Context) error {
	jar, err := loadCookies(c.cfg)
	if err != nil {
		return err
	}
	if !jar.IsLoggedIn() {
		return fmt.Errorf("Instagram cookies are missing a session (missing %v)", jar.GetMissingCookieNames())
	}
	logger := c.logger
	client := messagix.NewClient(jar, logger.With().Str("component", "messagix").Logger(), &messagix.Config{})

	media, err := coreclient.DialMedia(c.cfg.CoreAddress)
	if err != nil {
		return err
	}
	defer media.Close()

	inputs := make(chan *wuphfpb.ClientFrame, 128)
	codec := NewMediaCodec(media, http.DefaultClient, client.Instagram)
	normalizer := NewNormalizer(inputs, codec, jar.GetUserID(), logger)
	eventErrors := make(chan error, 1)
	client.SetEventHandler(func(ctx context.Context, raw any) {
		switch evt := raw.(type) {
		case *messagix.Event_PublishResponse:
			if err := normalizer.Handle(ctx, evt.Table); err != nil {
				reportEventError(eventErrors, fmt.Errorf("normalize Instagram event: %w", err))
			}
		case *messagix.Event_PermanentError:
			reportEventError(eventErrors, fmt.Errorf("Instagram connection failed permanently: %w", evt.Err))
		}
	})

	// Authenticate and load the account. The backlog messages are not replayed
	// (a restart must not re-bridge history), but the page's thread and contact
	// rows are primed into the title cache: incremental message deltas do not
	// carry thread names, so without this seed live events would have no
	// chat_title and mirror threads would never be named.
	_, initial, err := client.LoadMessagesPage(ctx)
	if err != nil {
		return fmt.Errorf("load Instagram account: %w", err)
	}
	if initial != nil {
		normalizer.cacheTitles(initial)
	}
	if err := client.Connect(ctx); err != nil {
		return fmt.Errorf("connect Instagram session: %w", err)
	}
	defer func() {
		client.Disconnect()
		if err := persistCookies(c.cfg.SessionFile, client.GetCookies()); err != nil {
			logger.Err(err).Msg("failed to persist refreshed Instagram cookies")
		}
	}()

	logger.Info().Msg("Instagram session connected")
	runResult := make(chan error, 1)
	go func() {
		runResult <- coreclient.New(c.cfg, logger).Run(ctx, NewSender(client), inputs)
	}()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-eventErrors:
		return err
	case err := <-runResult:
		return err
	}
}

func reportEventError(sink chan<- error, err error) {
	select {
	case sink <- err:
	default:
	}
}

// loadCookies reads the Instagram session cookies, preferring the persisted
// session file (which holds the latest refreshed cookies) and falling back to
// the configured export when no session has been saved yet.
func loadCookies(cfg config.File) (*cookies.Cookies, error) {
	path := cfg.SessionFile
	if _, err := os.Stat(path); errors.Is(err, os.ErrNotExist) {
		path = cfg.CookiesFile
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read Instagram cookies: %w", err)
	}
	jar := &cookies.Cookies{Platform: types.Instagram}
	if err := json.Unmarshal(data, jar); err != nil {
		return nil, fmt.Errorf("decode Instagram cookies: %w", err)
	}
	return jar, nil
}

func persistCookies(path string, jar *cookies.Cookies) error {
	data, err := json.Marshal(jar)
	if err != nil {
		return fmt.Errorf("encode Instagram cookies: %w", err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		return fmt.Errorf("write Instagram session: %w", err)
	}
	return nil
}
