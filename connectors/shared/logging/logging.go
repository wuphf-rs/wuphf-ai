// Package logging builds the connector's root zerolog logger. Every component
// derives its logger from this one (via With().Str("component", ...)) so all
// output shares one format and destination. The level is zerolog's global
// level, so SetLevel changes it for every derived logger at once (the log
// level is a runtime setting).
package logging

import (
	"fmt"
	"os"
	"slices"
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

// levels are the log levels a configuration may name (the connectors' runtime
// settings declare the same list).
var levels = []string{"trace", "debug", "info", "warn", "error"}

// CheckLevel reports a configured level that is not one of trace, debug, info,
// warn or error (in any case). Empty means info. The connectors check it when
// loading their file, because the core refuses a settings declaration whose
// level it does not know.
func CheckLevel(configured string) error {
	if configured == "" || slices.Contains(levels, strings.ToLower(configured)) {
		return nil
	}
	return fmt.Errorf("log_level must be one of %s, not %q", strings.Join(levels, ", "), configured)
}
