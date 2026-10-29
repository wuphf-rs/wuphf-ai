// Package logging builds the connector's root zerolog logger. Every component
// derives its logger from this one (via With().Str("component", ...)) so all
// output shares one format and destination. The level is zerolog's global
// level, so SetLevel changes it for every derived logger at once (the log
// level is a runtime setting).
package logging

import (
	"os"
	"strings"

	"github.com/rs/zerolog"
)

// envLevel overrides the configured log level without editing the config file,
// e.g. WUPHF_LOG_LEVEL=debug for a one-off verbose run.
const envLevel = "WUPHF_LOG_LEVEL"

// New builds the root logger and applies the configured level (see SetLevel).
func New(configured string) zerolog.Logger {
	SetLevel(configured)
	// The logger itself passes everything; the global level filters.
	return zerolog.New(zerolog.ConsoleWriter{Out: os.Stderr}).With().Timestamp().Logger()
}

// SetLevel makes configured the level of every logger. The environment
// variable takes precedence over it; an empty or unrecognized level falls back
// to info rather than failing.
func SetLevel(configured string) {
	zerolog.SetGlobalLevel(parseLevel(os.Getenv(envLevel), configured))
}

func parseLevel(candidates ...string) zerolog.Level {
	for _, candidate := range candidates {
		if candidate == "" {
			continue
		}
		if level, err := zerolog.ParseLevel(strings.ToLower(candidate)); err == nil {
			return level
		}
	}
	return zerolog.InfoLevel
}
