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

	"github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/akshettrj/wuphf/connectors/shared/setup"
	"github.com/akshettrj/wuphf/misc/internal/config"
	"github.com/akshettrj/wuphf/misc/internal/misc"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/propheci/botconf/go/tui"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
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
	if setup.Requested(os.Args[1:]) {
		if err := setup.Run(os.Args[2:], config.Description(), validate); err != nil {
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

	features, closeFeatures, err := buildFeatures(cfg, logger)
	if err != nil {
		exit(err)
	}
	defer closeFeatures()
	if len(features) == 0 {
		exit(errors.New("no feature is enabled; enable one in the config"))
	}
	for _, feature := range features {
		logger.Info().Str("feature", feature.Name()).Msg("feature enabled")
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	runner := misc.NewRunner(wuphfpb.NewObserverTransportClient(connection), features, cfg.ReconnectDelay(), logger)
	if err := runner.Run(ctx); err != nil && !errors.Is(err, context.Canceled) {
		exit(err)
	}
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-misc: %v\n", err)
	os.Exit(1)
}
