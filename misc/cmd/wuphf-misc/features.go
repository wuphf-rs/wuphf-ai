package main

import (
	"github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/shared/functions"
	"github.com/akshettrj/wuphf/misc/internal/config"
	"github.com/akshettrj/wuphf/misc/internal/misc"
	"github.com/akshettrj/wuphf/misc/internal/misc/instagram"
	"github.com/rs/zerolog"
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
