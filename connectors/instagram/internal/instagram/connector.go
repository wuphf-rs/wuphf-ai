package instagramconnector

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"

	"github.com/akshettrj/wuphf/connectors/instagram/internal/config"
	coreclient "github.com/akshettrj/wuphf/connectors/shared/coreclient"
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
			// messagix's connection loop has given up and returned, so nothing
			// will arrive on this session again. Report it to the owner rather
			// than failing the connector: the core session is what carries the
			// notice out through the other connectors, and tearing it down here
			// would race the notice off the wire. The connector is left idle
			// until it is restarted, which the notice says to do.
			logger.Err(evt.Err).Msg("Instagram connection failed permanently; connector is idle until restarted")
			kind, detail := sessionNotice(evt.Err)
			coreclient.SendNotice(ctx, inputs, kind, detail)
		case *messagix.Event_Ready, *messagix.Event_Reconnected:
			// Paired with the above: reaching the platform again tells the owner
			// the bridge is back. The core drops it when nothing was reported
			// wrong, so an ordinary connect stays quiet.
			coreclient.SendNotice(ctx, inputs, wuphfpb.NoticeKind_NOTICE_KIND_RECOVERED, "")
		}
	})

	// Authenticate and load the account. The page's thread and contact rows are
	// primed into the title cache up front: incremental message deltas do not
	// carry thread names, so without this seed live events would have no
	// chat_title and mirror threads would never be named. The inbox's messages
	// are replayed separately below when backfill is enabled.
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
		runResult <- coreclient.New(coreclient.Options{
			CoreAddress:         c.cfg.CoreAddress,
			ConnectorID:         c.cfg.ConnectorID,
			Platform:            wuphfpb.Platform_PLATFORM_INSTAGRAM,
			SupportsThreads:     c.cfg.SupportsThreads,
			SupportsTopicCreate: c.cfg.SupportsTopicCreate,
		}, logger).Run(ctx, NewSender(client), inputs)
	}()

	// Replay the inbox snapshot so messages exchanged while the connector was
	// down reach the mirror, the Instagram counterpart to WhatsApp's offline
	// history sync. The core dedups by native id (persisted in seen_events), so
	// already-bridged messages are dropped and only newly-missed ones are
	// mirrored; on a fresh install the recent inbox is bridged once. It runs
	// after the core consumer is started (so a large inbox does not block on the
	// input channel) and in the background (so a slow replay, e.g. media
	// downloads, does not delay live events). Live events dedup against it.
	if initial != nil && c.cfg.ShouldBackfill() {
		go func() {
			logger.Info().Msg("replaying Instagram inbox backlog")
			if err := normalizer.Handle(ctx, initial); err != nil {
				logger.Err(err).Msg("failed to replay Instagram inbox backlog")
			}
		}()
	}
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

// sessionNotice classifies a permanent Instagram connection failure for the
// owner. Instagram refuses the realtime connection outright once the session is
// no longer good, so the fix is nearly always fresh cookies rather than
// anything the connector can retry — messagix only reports this after it has
// already given up reconnecting.
//
// Transient socket errors are deliberately not reported: messagix retries them
// with backoff and they routinely clear on their own, so notifying would page
// the owner about an outage that fixes itself.
func sessionNotice(err error) (wuphfpb.NoticeKind, string) {
	detail := "Instagram refused the realtime connection"
	if err != nil {
		detail = fmt.Sprintf("%s: %v", detail, err)
	}
	return wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED, detail
}
