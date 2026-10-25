// Command wuphf-mediaconv serves media.convert@1 to the core: it converts
// animated stickers between the formats chat platforms use.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"

	"github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/shared/functions"
	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/akshettrj/wuphf/providers/mediaconv/internal/config"
	"github.com/akshettrj/wuphf/providers/mediaconv/internal/convert"
)

func main() {
	configPath := flag.String("config", "config.example.toml", "path to provider TOML configuration")
	flag.Parse()

	cfg, err := config.Load(*configPath)
	if err != nil {
		exit(err)
	}
	logger := logging.New(cfg.LogLevel)
	token := os.Getenv(cfg.TokenEnv)
	if token == "" {
		exit(fmt.Errorf("environment variable %s (token_env) is unset or empty", cfg.TokenEnv))
	}
	media, err := coreclient.DialMedia(cfg.CoreAddress, token)
	if err != nil {
		exit(err)
	}
	defer media.Close()

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	logger.Info().Str("core", cfg.CoreAddress).Str("provider", cfg.ProviderID).Msg("serving media.convert@1")
	err = functions.Serve(ctx, functions.ProviderOptions{
		Address:        cfg.CoreAddress,
		ProviderID:     cfg.ProviderID,
		Token:          token,
		Functions:      []functions.Function{convert.Function(media, cfg.Concurrency(), cfg.DefaultDeadline())},
		ReconnectDelay: cfg.ReconnectDelay(),
	}, logger)
	if err != nil && !errors.Is(err, context.Canceled) {
		exit(err)
	}
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-mediaconv: %v\n", err)
	os.Exit(1)
}
