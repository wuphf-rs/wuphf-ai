// Package logging builds the connector's root zerolog logger. Every component
// derives its logger from this one (via With().Str("component", ...)) so all
// output shares one format, destination, and level switch.
package logging

import (
	"os"
	"strings"

	"github.com/rs/zerolog"
)

// envLevel overrides the configured log level without editing the config file,
// e.g. WUPHF_LOG_LEVEL=debug for a one-off verbose run.
const envLevel = "WUPHF_LOG_LEVEL"

// New builds the root logger at the requested level. The environment variable
// takes precedence over the configured value; an empty or unrecognized level
// falls back to info rather than failing startup.
func New(configured string) zerolog.Logger {
	logger := zerolog.New(zerolog.ConsoleWriter{Out: os.Stderr}).With().Timestamp().Logger()
	return logger.Level(parseLevel(os.Getenv(envLevel), configured))
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
