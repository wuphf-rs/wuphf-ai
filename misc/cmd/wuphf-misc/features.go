package main

import (
	"github.com/akshettrj/wuphf/misc/internal/config"
	"github.com/akshettrj/wuphf/misc/internal/misc"
	"github.com/rs/zerolog"
)

// buildFeatures returns the enabled features and a function that releases
// what they hold.
func buildFeatures(cfg config.File, logger zerolog.Logger) ([]misc.Feature, func(), error) {
	return nil, func() {}, nil
}
