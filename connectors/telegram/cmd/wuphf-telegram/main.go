package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"

	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	telegramconnector "github.com/akshettrj/wuphf/connectors/telegram/internal/telegram"
)

func main() {
	// The first non-flag argument selects a subcommand; absent one, the
	// connector runs normally. Each subcommand owns its own flag set.
	args := os.Args[1:]
	command := "run"
	if len(args) > 0 && args[0] != "" && args[0][0] != '-' {
		command = args[0]
		args = args[1:]
	}

	switch command {
	case "run":
		runConnector(args)
	case "reset":
		runReset(args)
	default:
		exit(fmt.Errorf("unknown command %q (expected \"run\" or \"reset\")", command))
	}
}

func runConnector(args []string) {
	flags := flag.NewFlagSet("run", flag.ExitOnError)
	configPath := flags.String("config", "config.example.toml", "path to connector TOML configuration")
	_ = flags.Parse(args)

	connector, err := newConnector(*configPath)
	if err != nil {
		exit(err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	if err := connector.Run(ctx); err != nil {
		exit(err)
	}
}

func runReset(args []string) {
	flags := flag.NewFlagSet("reset", flag.ExitOnError)
	configPath := flags.String("config", "config.example.toml", "path to connector TOML configuration")
	chatID := flags.String("chat", "", "target chat id to reset (e.g. telegram:channel:123)")
	confirm := flags.Bool("yes", false, "skip the confirmation prompt")
	_ = flags.Parse(args)

	if *chatID == "" {
		exit(fmt.Errorf("reset requires -chat"))
	}

	connector, err := newConnector(*configPath)
	if err != nil {
		exit(err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	if err := connector.Reset(ctx, *chatID, *confirm); err != nil {
		exit(err)
	}
}

func newConnector(configPath string) (telegramconnector.Connector, error) {
	cfg, err := config.Load(configPath)
	if err != nil {
		return telegramconnector.Connector{}, err
	}
	credentials, err := config.LoadCredentials()
	if err != nil {
		return telegramconnector.Connector{}, err
	}
	return telegramconnector.New(cfg, credentials, logging.New(cfg.LogLevel)), nil
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-telegram: %v\n", err)
	os.Exit(1)
}
