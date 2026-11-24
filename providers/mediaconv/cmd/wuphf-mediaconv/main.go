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

	"github.com/propheci/botconf/go/tui"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/functions"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/setup"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/version"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"github.com/wuphf-rs/wuphf-ai/providers/mediaconv/internal/config"
	"github.com/wuphf-rs/wuphf-ai/providers/mediaconv/internal/convert"
)

// validate is setup's check of a candidate config: what the provider checks
// at startup (the file, then its token).
func validate(candidate tui.Candidate) error {
	cfg, err := config.Parse([]byte(candidate.Config))
	if err != nil {
		return err
	}
	return setup.RequireToken(candidate, "token_env", cfg.TokenEnv)
}

func main() {
	if version.Requested(os.Args[1:]) {
		version.Print("wuphf-mediaconv")
		return
	}
	if setup.Requested(os.Args[1:]) {
		if err := setup.Run(os.Args[2:], tui.Setup{Description: config.Description(), Validate: validate}); err != nil {
			exit(err)
		}
		return
	}
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

// mediaTools are the programs media.convert runs, and whether all are there.
// ImageMagick is `magick` (v7) or the legacy `convert`.
func mediaTools() ([]*wuphfpb.ToolStatus, bool) {
	const neededFor = "animated stickers to WhatsApp (they arrive as files)"
	ffmpeg := coreclient.ToolStatus("ffmpeg", neededFor)
	magick := coreclient.ToolStatus("magick", neededFor)
	if !magick.GetAvailable() && coreclient.ToolStatus("convert", "").GetAvailable() {
		magick.Available = true
	}
	return []*wuphfpb.ToolStatus{ffmpeg, magick}, ffmpeg.GetAvailable() && magick.GetAvailable()
}
