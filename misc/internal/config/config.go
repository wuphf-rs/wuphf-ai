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
	"github.com/propheci/botconf/go/describe"
)

// File is wuphf-misc's config file. Each option is described once, in its
// tags (see the shared setup package); config.example.toml is generated from
// them.
type File struct {
	CoreAddress string `toml:"core_address" required:"true" example:"127.0.0.1:50051" doc:"The core's gRPC address."`
	// The token itself must equal the core's observer_token, so it is entered,
	// never generated here.
	ObserverTokenEnv string `toml:"observer_token_env" required:"true" env:"secret" example:"WUPHF_OBSERVER_TOKEN" doc:"Environment variable holding the core's observer token (the core's observer_token). This token is full access: it reads every captured chat and sends as you into any of them."`
	InvokeTokenEnv   string `toml:"invoke_token_env" env:"token" hint:"WUPHF_INVOKE_TOKEN" doc:"Environment variable holding the core's functions invoke token, when the core sets [functions] invoke_token_env."`
	LogLevel         string `toml:"log_level" example:"info" jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error" doc:"How much wuphf-misc logs (WUPHF_LOG_LEVEL wins). Empty means info."`
	// 0 means DefaultReconnectDelay.
	ReconnectDelayMs uint32 `toml:"reconnect_delay_ms" hint:"2000" doc:"Pause before resubscribing after the message stream ends, in milliseconds; 0 means 2000."`

	Instagram Instagram `toml:"instagram"`
}

// Description is the config file's description, from File's tags.
func Description() *describe.Description {
	return describe.MustOf("wuphf-misc", "Features that are not bridging. wuphf-misc follows the core's "+
		"live messages through the observer service and replies through it. A change to this file "+
		"needs a restart.", File{})
}

// Instagram configures the Instagram downloader: a post or reel link posted
// in one of Chats is answered with the post's media. Zero values stand for
// the defaults (see WithDefaults).
type Instagram struct {
	Enabled bool     `toml:"enabled" example:"false" doc:"Instagram downloader: when someone posts an Instagram post or reel link in one of chats, reply to it with the post's video or photos (a carousel's every item). Messages you send count too, from your phone or the TUI; wuphf-misc's own replies never do. Edits do not. Links sent while wuphf-misc is down are not answered.\n\nThe fetch runs on the Instagram connector's logged-in account, through the core's function broker: enable the Instagram connector's [functions] section and the core's matching [[functions.providers]] entry. Every fetch is an Instagram request on that account, so keep the limits low."`
	Chats   []string `toml:"chats" example:"[\"whatsapp-main:120363000000000000@g.us\"]" doc:"Chat references, as in the core's rules: connector:chat, or connector:chat#thread for one thread only. Required when enabled."`
	// Nil means true.
	Caption            *bool  `toml:"caption" default:"true" example:"true" doc:"Add \"@author: \" and up to 200 characters of the post's text to the reply."`
	ReplyOnFailure     bool   `toml:"reply_on_failure" example:"false" doc:"Reply \"Could not download this post.\" when a fetch fails; else a failure is only logged."`
	DedupHours         uint32 `toml:"dedup_hours" default:"24" example:"24" doc:"The same post in the same chat is answered once in this many hours."`
	MaxPerMinute       uint32 `toml:"max_per_minute" default:"10" example:"10" doc:"At most this many fetches a minute, over every chat; more are skipped and logged."`
	MaxConcurrent      uint32 `toml:"max_concurrent" default:"2" example:"2" doc:"At most this many fetches at a time."`
	CallTimeoutSeconds uint32 `toml:"call_timeout_seconds" default:"60" example:"60" doc:"One fetch's time limit, in seconds, waiting for the Instagram connector included."`
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
