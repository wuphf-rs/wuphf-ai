package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"

	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/akshettrj/wuphf/connectors/shared/setup"
	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	telegramconnector "github.com/akshettrj/wuphf/connectors/telegram/internal/telegram"
	"github.com/propheci/botconf/go/tui"
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
	case "setup":
		if err := setup.Run(args, tui.Setup{
			Description: config.Description(),
			Validate:    validate,
			Actions:     setup.AccountActions(chatdir.Dir(), openAccount),
		}); err != nil {
			exit(err)
		}
	default:
		exit(fmt.Errorf("unknown command %q (expected \"run\", \"reset\" or \"setup\")", command))
	}
}

// validate is setup's check of a candidate config: what the connector checks
// at startup (the file, its core token, the application credentials on the
// Environment screen). The phone may still come at Link account.
func validate(candidate tui.Candidate) error {
	cfg, err := config.Parse([]byte(candidate.Config))
	if err != nil {
		return err
	}
	if err := setup.RequireToken(candidate, "core_token_env", cfg.CoreTokenEnv); err != nil {
		return err
	}
	if _, err := config.ReadCredentials(cfg, func(name string) string { return setup.Env(candidate, name) }); err != nil {
		return fmt.Errorf("%w (set it on the Environment screen)", err)
	}
	return nil
}

// openAccount is setup's account on the saved config, its credentials read
// from the env file being edited before the environment.
func openAccount(candidate tui.Candidate) (*setup.Account, error) {
	cfg, err := config.Parse([]byte(candidate.Config))
	if err != nil {
		return nil, err
	}
	credentials, err := config.ReadCredentials(cfg, func(name string) string { return setup.Env(candidate, name) })
	if err != nil {
		return nil, err
	}
	return telegramconnector.SetupAccount(cfg, credentials), nil
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
	credentials, err := config.LoadCredentials(cfg)
	if err != nil {
		return telegramconnector.Connector{}, err
	}
	return telegramconnector.New(cfg, credentials, logging.New(cfg.LogLevel)), nil
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-telegram: %v\n", err)
	os.Exit(1)
}
