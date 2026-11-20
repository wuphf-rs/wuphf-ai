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
	"github.com/akshettrj/wuphf/connectors/shared/functions"
	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/akshettrj/wuphf/connectors/shared/settings"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/instameow"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/util/exhttp"
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

	coreToken, err := coreclient.TokenFromEnv(c.cfg.CoreTokenEnv)
	if err != nil {
		return err
	}
	media, err := coreclient.DialMedia(c.cfg.CoreAddress, coreToken)
	if err != nil {
		return err
	}
	defer media.Close()
	// The runtime settings: declared to the core when registering, then
	// followed as the core pushes changes.
	runtime, err := settings.Connect(c.cfg.CoreAddress, coreToken, c.cfg.ConnectorID, c.cfg.Runtime.WithDefaults(), logger)
	if err != nil {
		return err
	}
	defer runtime.Close()
	go runtime.Follow(ctx, func(old, new config.Runtime) {
		if old.LogLevel != new.LogLevel {
			logging.SetLevel(new.LogLevel)
		}
	})

	client := instameow.NewClient(instameow.ClientParams{
		Cookies:  jar,
		Log:      logger.With().Str("component", "instameow").Logger(),
		Settings: exhttp.SensibleClientSettings,
		// Typing indicators are not bridged; skip their extra socket.
		DisableTyping: true,
	})
	defer func() {
		client.Disconnect()
		if err := persistCookies(c.cfg.SessionFile, client.GetCookies()); err != nil {
			logger.Err(err).Msg("failed to persist refreshed Instagram cookies")
		}
	}()

	inputs := make(chan *wuphfpb.ClientFrame, 128)
	ig := &session{
		client:     client,
		state:      NewStateStore(c.cfg.StatePath()),
		inputs:     inputs,
		logger:     logger,
		resnapshot: make(chan struct{}, 1),
	}
	// Set before loading so an auth failure during load reaches the owner.
	// Deltas only flow once the stream connects, after the normalizer exists.
	client.SetEventHandler(ig.handleEvent)
	mailbox, err := ig.load(ctx, c.cfg.ShouldBackfill())
	if err != nil {
		return err
	}
	// The owner's id is only known once the account is loaded; it labels the
	// owner's own messages and reactions.
	ownUserID := client.GetOwnFBID()
	if ownUserID == 0 {
		logger.Warn().Msg("Instagram account id unknown; own messages will not be labelled")
	}
	ig.threads = NewThreadDirectory(client, ownUserID)
	ig.threads.RecordMailbox(mailbox)
	codec := NewMediaCodec(media, http.DefaultClient, client)
	ig.normalizer = NewNormalizer(inputs, codec, ig.threads, ownUserID, logger)
	logger.Info().Msg("Instagram account loaded")

	if c.cfg.Functions != nil {
		if err := c.serveFunctions(ctx, codec, logger); err != nil {
			return err
		}
	}

	runResult := make(chan error, 1)
	go func() {
		runResult <- coreclient.New(coreclient.Options{
			CoreAddress:         c.cfg.CoreAddress,
			Token:               coreToken,
			ConnectorID:         c.cfg.ConnectorID,
			Platform:            wuphfpb.Platform_PLATFORM_INSTAGRAM,
			SupportsThreads:     c.cfg.SupportsThreads,
			SupportsTopicCreate: c.cfg.SupportsTopicCreate,
			Settings:            runtime.Declaration,
		}, logger).Run(ctx, NewSender(client, ig.threads), inputs)
	}()
	// The stream starts after the core consumer, so a burst of replayed
	// deltas does not block on the input channel before anything drains it.
	go ig.run(ctx)

	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-runResult:
		return err
	}
}

// serveFunctions serves instagram.fetch_media@1 on the logged-in session until
// ctx ends. The provider session is separate from the connector session: a
// provider the core rejects is logged, and bridging goes on without it.
func (c Connector) serveFunctions(ctx context.Context, codec MediaCodec, logger zerolog.Logger) error {
	cfg := *c.cfg.Functions
	token, err := coreclient.TokenFromEnv(cfg.TokenEnv)
	if err != nil {
		return err
	}
	providerLogger := logger.With().Str("provider", cfg.ProviderID).Logger()
	go func() {
		err := functions.Serve(ctx, functions.ProviderOptions{
			Address:    c.cfg.CoreAddress,
			ProviderID: cfg.ProviderID,
			Token:      token,
			Functions:  []functions.Function{FetchMediaFunction(codec, cfg.Concurrency(), providerLogger)},
		}, providerLogger)
		if err != nil && !errors.Is(err, context.Canceled) {
			providerLogger.Err(err).Msg("instagram.fetch_media@1 is not served")
		}
	}()
	providerLogger.Info().Msg("serving instagram.fetch_media@1")
	return nil
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
