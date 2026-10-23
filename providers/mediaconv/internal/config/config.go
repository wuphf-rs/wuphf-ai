// Package config loads the mediaconv provider's TOML configuration.
package config

import (
	"errors"
	"fmt"
	"os"
	"runtime"
	"time"

	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress string `toml:"core_address"`
	ProviderID  string `toml:"provider_id"`
	// TokenEnv names the environment variable holding this provider's token;
	// it must match the core's [[functions.providers]] entry.
	TokenEnv string `toml:"token_env"`
	// LogLevel sets log verbosity (trace, debug, info, warn, error). Empty
	// means info; the WUPHF_LOG_LEVEL environment variable overrides it.
	LogLevel string `toml:"log_level"`
	// MaxConcurrency bounds simultaneous conversions; 0 = runtime.NumCPU().
	MaxConcurrency uint32 `toml:"max_concurrency"`
	// DefaultDeadlineMs is the deadline suggested to the core; 0 = core default.
	DefaultDeadlineMs uint32 `toml:"default_deadline_ms"`
	// ReconnectDelayMs is the pause before reconnecting to the core; 0 = 2000.
	ReconnectDelayMs uint32 `toml:"reconnect_delay_ms"`
}

func Load(path string) (File, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return File{}, fmt.Errorf("read configuration: %w", err)
	}
	var cfg File
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return File{}, fmt.Errorf("decode configuration: %w", err)
	}
	if err := cfg.Validate(); err != nil {
		return File{}, err
	}
	return cfg, nil
}

// Validate requires core_address, provider_id and token_env.
func (c File) Validate() error {
	if c.CoreAddress == "" || c.ProviderID == "" || c.TokenEnv == "" {
		return errors.New("core_address, provider_id, and token_env are required")
	}
	return nil
}

// Concurrency is the number of conversions to run at once.
func (c File) Concurrency() uint32 {
	if c.MaxConcurrency == 0 {
		return uint32(runtime.NumCPU())
	}
	return c.MaxConcurrency
}

// DefaultDeadline is the deadline suggested to the core; zero leaves it to
// the core.
func (c File) DefaultDeadline() time.Duration {
	return time.Duration(c.DefaultDeadlineMs) * time.Millisecond
}

// ReconnectDelay is the pause before reconnecting to the core; zero lets the
// SDK use its default.
func (c File) ReconnectDelay() time.Duration {
	return time.Duration(c.ReconnectDelayMs) * time.Millisecond
}
