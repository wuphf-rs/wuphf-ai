package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"

	"github.com/propheci/botconf/go/tui"
	"github.com/rs/zerolog"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/chatdir"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/setup"
	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
	whatsappconnector "github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/whatsapp"
)

func main() {
	if setup.Requested(os.Args[1:]) {
		if err := setup.Run(os.Args[2:], tui.Setup{
			Description: config.Description(),
			Validate:    validate,
			Actions:     setup.AccountActions(chatdir.Dir(), openAccount),
		}); err != nil {
			exit(err)
		}
		return
	}
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

// validate is setup's check of a candidate config: what the connector checks
// at startup (the file, then its core token).
func validate(candidate tui.Candidate) error {
	cfg, err := config.Parse([]byte(candidate.Config))
	if err != nil {
		return err
	}
	return setup.RequireToken(candidate, "core_token_env", cfg.CoreTokenEnv)
}

// openAccount is setup's account on the saved config. whatsmeow logs
// nowhere: the terminal is the setup screen.
func openAccount(candidate tui.Candidate) (*setup.Account, error) {
	cfg, err := config.Parse([]byte(candidate.Config))
	if err != nil {
		return nil, err
	}
	return whatsappconnector.SetupAccount(cfg, zerolog.Nop()), nil
}

func exit(err error) {
	fmt.Fprintf(os.Stderr, "wuphf-whatsapp: %v\n", err)
	os.Exit(1)
}
