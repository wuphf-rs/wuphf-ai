package instagramconnector

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
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
	cfg config.File
}

func New(cfg config.File) Connector {
	return Connector{cfg: cfg}
}

func (c Connector) Run(ctx context.Context) error {
	jar, err := loadCookies(c.cfg)
	if err != nil {
		return err
	}
	if !jar.IsLoggedIn() {
		return fmt.Errorf("Instagram cookies are missing a session (missing %v)", jar.GetMissingCookieNames())
	}
	logger := zerolog.New(zerolog.ConsoleWriter{Out: os.Stderr}).With().Timestamp().Logger()
	client := messagix.NewClient(jar, logger, &messagix.Config{})

	media, err := coreclient.DialMedia(c.cfg.CoreAddress)
	if err != nil {
		return err
	}
	defer media.Close()

	inputs := make(chan *wuphfpb.ClientFrame, 128)
	normalizer := NewNormalizer(inputs)
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

	// Authenticate and load the account; the returned backlog table is ignored so
	// a restart does not replay already-bridged history.
	if _, _, err := client.LoadMessagesPage(ctx); err != nil {
		return fmt.Errorf("load Instagram account: %w", err)
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

	runResult := make(chan error, 1)
	go func() {
		runResult <- coreclient.New(c.cfg).Run(ctx, NewSender(client), inputs)
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
