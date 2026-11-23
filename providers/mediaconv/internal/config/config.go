// Package config loads the mediaconv provider's TOML configuration.
package config

import (
	"errors"
	"fmt"
	"os"
	"runtime"
	"time"

	"github.com/pelletier/go-toml/v2"
	"github.com/propheci/botconf/go/describe"
)

// File is the provider's config file. Each option is described once, in its
// tags (see the shared setup package); config.example.toml is generated from
// them.
type File struct {
	CoreAddress string `toml:"core_address" required:"true" example:"127.0.0.1:50051" doc:"The core's gRPC address."`
	ProviderID  string `toml:"provider_id" required:"true" example:"mediaconv" doc:"Must match a [[functions.providers]] id in the core configuration, whose functions list must allow \"media.convert\"."`
	// TokenEnv names the variable; the token itself is read from the
	// environment at startup.
	TokenEnv string `toml:"token_env" required:"true" env:"token" example:"WUPHF_PROVIDER_MEDIACONV_TOKEN" doc:"Environment variable holding this provider's token; the core reads the same token from the variable named by its own [[functions.providers]] token_env."`
	LogLevel string `toml:"log_level" example:"info" jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error" doc:"How much the provider logs (WUPHF_LOG_LEVEL wins). Empty means info."`
	// 0 means runtime.NumCPU().
	MaxConcurrency    uint32 `toml:"max_concurrency" doc:"Conversions that run at once; 0 means the number of CPUs."`
	DefaultDeadlineMs uint32 `toml:"default_deadline_ms" doc:"The deadline suggested to the core for a conversion, in milliseconds; 0 leaves it to the core."`
	ReconnectDelayMs  uint32 `toml:"reconnect_delay_ms" hint:"2000" doc:"The pause before reconnecting to the core, in milliseconds; 0 means 2000."`
}

// Description is the config file's description, from File's tags.
func Description() *describe.Description {
	return describe.MustOf("wuphf media converter (media.convert@1 provider)", "", File{})
}

func Load(path string) (File, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return File{}, fmt.Errorf("read configuration: %w", err)
	}
	return Parse(data)
}

// Parse reads and checks a configuration's text.
func Parse(data []byte) (File, error) {
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
