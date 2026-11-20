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

	// Instagram configures the Instagram downloader feature.
	Instagram Instagram `toml:"instagram"`
}

// Instagram configures the Instagram downloader: a post or reel link posted
// in one of Chats is answered with the post's media.
type Instagram struct {
	Enabled bool `toml:"enabled"`
	// Chats are chat references (`connector:chat`, or `connector:chat#thread`
	// for one thread) in the form the core's rules use. Required when enabled.
	Chats []string `toml:"chats"`
	// Caption adds the author and up to CaptionMaxChars of the post's text to
	// the reply. Nil means true.
	Caption *bool `toml:"caption"`
	// ReplyOnFailure replies "Could not download this post." when a fetch
	// fails; otherwise a failure is only logged.
	ReplyOnFailure bool `toml:"reply_on_failure"`
	// DedupHours: the same post in the same chat is answered once in this
	// window. 0 = 24.
	DedupHours uint32 `toml:"dedup_hours"`
	// MaxPerMinute caps fetches per minute across every chat; links over it
	// are skipped and logged. 0 = 10.
	MaxPerMinute uint32 `toml:"max_per_minute"`
	// MaxConcurrent caps fetches at once. 0 = 2.
	MaxConcurrent uint32 `toml:"max_concurrent"`
	// CallTimeoutSeconds bounds one fetch, waiting for the Instagram
	// connector included. 0 = 60.
	CallTimeoutSeconds uint32 `toml:"call_timeout_seconds"`
}

// CaptionMaxChars is the most post text a reply's caption carries.
const CaptionMaxChars = 200

// WithDefaults fills in what each zero value stands for.
func (c Instagram) WithDefaults() Instagram {
	if c.Caption == nil {
		enabled := true
		c.Caption = &enabled
	}
	if c.DedupHours == 0 {
		c.DedupHours = 24
	}
	if c.MaxPerMinute == 0 {
		c.MaxPerMinute = 10
	}
	if c.MaxConcurrent == 0 {
		c.MaxConcurrent = 2
	}
	if c.CallTimeoutSeconds == 0 {
		c.CallTimeoutSeconds = 60
	}
	return c
}

func (c Instagram) Dedup() time.Duration {
	return time.Duration(c.DedupHours) * time.Hour
}

func (c Instagram) CallTimeout() time.Duration {
	return time.Duration(c.CallTimeoutSeconds) * time.Second
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
	if c.Instagram.Enabled && len(c.Instagram.Chats) == 0 {
		return errors.New("[instagram] is enabled but lists no chats")
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
