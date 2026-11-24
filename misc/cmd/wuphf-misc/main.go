// Command wuphf-misc runs the features that are not bridging (see
// internal/misc): it follows the core's live messages through the observer
// service and replies through it.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"slices"
	"time"

	"github.com/propheci/botconf/go/tui"
	"github.com/rs/zerolog"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/chatdir"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/setup"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/version"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/config"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/misc"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
)

// validate is setup's check of a candidate config: what wuphf-misc checks at
// startup (the file, then the observer token).
func validate(candidate tui.Candidate) error {
	cfg, err := config.Parse([]byte(candidate.Config))
	if err != nil {
		return err
	}
	return setup.RequireToken(candidate, "observer_token_env", cfg.ObserverTokenEnv)
}

func main() {
	if version.Requested(os.Args[1:]) {
		version.Print("wuphf-misc")
		return
	}
	if setup.Requested(os.Args[1:]) {
		if err := setup.Run(os.Args[2:], tui.Setup{
			Description: config.Description(),
			Validate:    validate,
			// [instagram] chats are picked by name from the chat directory.
			Suggest: setup.SuggestChats(chatdir.Dir()),
		}); err != nil {
			exit(err)
		}
		return
	}
	configPath := flag.String("config", "config.example.toml", "path to wuphf-misc TOML configuration")
	flag.Parse()

	cfg, err := config.Load(*configPath)
	if err != nil {
		exit(err)
	}
	logger := logging.New(cfg.LogLevel)
	observerToken, err := coreclient.TokenFromEnv(cfg.ObserverTokenEnv)
	if err != nil {
		exit(err)
	}
	connection, err := grpc.NewClient(cfg.CoreAddress,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		coreclient.WithToken(observerToken),
	)
	if err != nil {
		exit(fmt.Errorf("connect to core observer service: %w", err))
	}
	defer connection.Close()

	features, err := buildFeatures(cfg, logger)
	if err != nil {
		exit(err)
	}
	defer features.Close()
	base := cfg.Runtime.WithDefaults()
	logEnabled(logger, base)

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	go followSettings(ctx, wuphfpb.NewPluginTransportClient(connection), cfg, observerToken, base, features, logger)
	runner := misc.NewRunner(wuphfpb.NewObserverTransportClient(connection), features.list, cfg.ReconnectDelay(), logger)
	if err := runner.Run(ctx); err != nil && !errors.Is(err, context.Canceled) {
		exit(err)
	}
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-misc: %v\n", err)
	os.Exit(1)
}

// declareTimeout bounds one attempt at declaring the settings to the core.
const declareTimeout = 10 * time.Second

// followSettings declares wuphf-misc's runtime settings to the core and
// applies every change to features until ctx ends. While the core cannot be
// reached it tries again; when the core refuses the declaration (a core
// without plugin settings, or no observer token), wuphf-misc runs on its
// file's values.
func followSettings(
	ctx context.Context,
	declarer settings.Declarer,
	cfg config.File,
	observerToken string,
	base config.Runtime,
	features *features,
	logger zerolog.Logger,
) {
	var connection *settings.Connection[config.Runtime]
	for connection == nil {
		attempt, cancel := context.WithTimeout(ctx, declareTimeout)
		connected, err := settings.ConnectPlugin(attempt, declarer, cfg.CoreAddress, observerToken, cfg.Plugin(), base, logger)
		cancel()
		switch {
		case err == nil:
			connection = connected
		case ctx.Err() != nil:
			return
		case retryable(err):
			logger.Debug().Err(err).Msg("core not reachable to declare the settings; retrying")
			select {
			case <-time.After(cfg.ReconnectDelay()):
			case <-ctx.Done():
				return
			}
		default:
			logger.Warn().Err(err).Msg("the core refused wuphf-misc's settings: running on the config file's values, which a change from the settings panel cannot reach")
			return
		}
	}
	defer connection.Close()
	logger.Info().Str("section", settings.PluginPrefix(cfg.Plugin())).Msg("settings declared; following changes from the settings panel")
	connection.Follow(ctx, func(old, new config.Runtime) {
		if old.LogLevel != new.LogLevel {
			logging.SetLevel(new.LogLevel)
		}
		features.Configure(new)
		if !slices.Equal(enabledFeatures(old), enabledFeatures(new)) {
			logEnabled(logger, new)
		}
	})
}

// retryable reports whether declaring may work later: the core is down or
// still starting.
func retryable(err error) bool {
	switch status.Code(err) {
	case codes.Unavailable, codes.DeadlineExceeded:
		return true
	}
	return false
}

// logEnabled says which features runtime turns on.
func logEnabled(logger zerolog.Logger, runtime config.Runtime) {
	enabled := enabledFeatures(runtime)
	if len(enabled) == 0 {
		logger.Info().Msg("no feature is enabled; enable one in the config or the settings panel")
		return
	}
	for _, name := range enabled {
		logger.Info().Str("feature", name).Msg("feature enabled")
	}
}
