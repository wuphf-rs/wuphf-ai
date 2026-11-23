package main

import (
	"github.com/rs/zerolog"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/functions"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/config"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/misc"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/misc/instagram"
)

// buildFeatures returns the enabled features and a function that releases
// what they hold.
func buildFeatures(cfg config.File, logger zerolog.Logger) ([]misc.Feature, func(), error) {
	var features []misc.Feature
	closers := []func(){}
	closeAll := func() {
		for _, release := range closers {
			release()
		}
	}
	if cfg.Instagram.Enabled {
		invokeToken := ""
		if cfg.InvokeTokenEnv != "" {
			token, err := coreclient.TokenFromEnv(cfg.InvokeTokenEnv)
			if err != nil {
				return nil, closeAll, err
			}
			invokeToken = token
		}
		client, err := functions.Dial(cfg.CoreAddress, invokeToken, misc.ClientName)
		if err != nil {
			return nil, closeAll, err
		}
		closers = append(closers, func() { _ = client.Close() })
		feature, err := instagram.New(cfg.Instagram, client, logger)
		if err != nil {
			return nil, closeAll, err
		}
		features = append(features, feature)
	}
	return features, closeAll, nil
}
