package main

import (
	"github.com/rs/zerolog"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/functions"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/config"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/misc"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/misc/instagram"
)

// features is every feature, enabled or not: each can be turned on from the
// settings panel while wuphf-misc runs.
type features struct {
	list []misc.Feature
	// configure applies new runtime settings to each feature.
	configure []func(config.Runtime)
	closers   []func()
}

// buildFeatures builds every feature from the file's settings.
func buildFeatures(cfg config.File, logger zerolog.Logger) (*features, error) {
	built := &features{}
	invokeToken := ""
	if cfg.InvokeTokenEnv != "" {
		token, err := coreclient.TokenFromEnv(cfg.InvokeTokenEnv)
		if err != nil {
			return nil, err
		}
		invokeToken = token
	}
	// The connection is made on the first call.
	client, err := functions.Dial(cfg.CoreAddress, invokeToken, misc.ClientName)
	if err != nil {
		return nil, err
	}
	built.closers = append(built.closers, func() { _ = client.Close() })
	downloader := instagram.New(cfg.Instagram, client, logger)
	built.list = append(built.list, downloader)
	built.configure = append(built.configure, func(runtime config.Runtime) {
		downloader.Configure(runtime.Instagram)
	})
	return built, nil
}

// Configure applies runtime to every feature, for the next message.
func (f *features) Configure(runtime config.Runtime) {
	for _, configure := range f.configure {
		configure(runtime)
	}
}

// enabledFeatures names the features runtime enables.
func enabledFeatures(runtime config.Runtime) []string {
	var names []string
	if runtime.Instagram.Enabled {
		names = append(names, "instagram")
	}
	return names
}

// Close releases what the features hold.
func (f *features) Close() {
	for _, release := range f.closers {
		release()
	}
}
