// Package config loads wuphf-misc's TOML configuration: how to reach the core,
// and one table per feature. A change needs a restart.
package config

import (
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress string `toml:"core_address"`
	// ObserverTokenEnv names the environment variable holding the core's
	// observer token. The observer token reads every captured chat and sends
	// as the owner into any of them. Required.
	ObserverTokenEnv string `toml:"observer_token_env"`
	// InvokeTokenEnv names the environment variable holding the core's
	// functions.invoke_token, when the core sets one. Optional.
	InvokeTokenEnv string `toml:"invoke_token_env"`
	// LogLevel sets log verbosity (trace, debug, info, warn, error). Empty
	// means info; the WUPHF_LOG_LEVEL environment variable overrides it.
	LogLevel string `toml:"log_level"`
	// ReconnectDelayMs is the pause before resubscribing after the message
	// stream ends; 0 = DefaultReconnectDelay.
	ReconnectDelayMs uint32 `toml:"reconnect_delay_ms"`
}

// DefaultReconnectDelay is the resubscribe pause when reconnect_delay_ms is unset.
const DefaultReconnectDelay = 2 * time.Second

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

func (c File) Validate() error {
	if err := logging.CheckLevel(c.LogLevel); err != nil {
		return err
	}
	if c.CoreAddress == "" || c.ObserverTokenEnv == "" {
		return errors.New("core_address and observer_token_env are required")
	}
	return nil
}

// ReconnectDelay is the pause before resubscribing to the message stream.
func (c File) ReconnectDelay() time.Duration {
	if c.ReconnectDelayMs == 0 {
		return DefaultReconnectDelay
	}
	return time.Duration(c.ReconnectDelayMs) * time.Millisecond
}
