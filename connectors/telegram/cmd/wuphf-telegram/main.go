package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"

	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	telegramconnector "github.com/akshettrj/wuphf/connectors/telegram/internal/telegram"
)

func main() {
	configPath := flag.String("config", "config.example.toml", "path to connector TOML configuration")
	flag.Parse()

	cfg, err := config.Load(*configPath)
	if err != nil {
		exit(err)
	}
	credentials, err := config.LoadCredentials()
	if err != nil {
		exit(err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	if err := telegramconnector.New(cfg, credentials).Run(ctx); err != nil {
		exit(err)
	}
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-telegram: %v\n", err)
	os.Exit(1)
}
