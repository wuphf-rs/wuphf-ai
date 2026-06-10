package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/logging"
	whatsappconnector "github.com/akshettrj/wuphf/connectors/whatsapp/internal/whatsapp"
)

func main() {
	configPath := flag.String("config", "config.example.toml", "path to connector TOML configuration")
	flag.Parse()
	cfg, err := config.Load(*configPath)
	if err != nil {
		exit(err)
	}
	logger := logging.New(cfg.LogLevel)
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	if err := whatsappconnector.New(cfg, logger).Run(ctx); err != nil {
		logger.Error().Err(err).Msg("connector stopped")
		os.Exit(1)
	}
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-whatsapp: %v\n", err)
	os.Exit(1)
}
