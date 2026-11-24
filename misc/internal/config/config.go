// Package config loads wuphf-misc's TOML configuration: how to reach the core,
// and one table per feature. The features' settings and the log level are
// runtime settings (Runtime): wuphf-misc declares them to the core, and a
// change from the settings panel applies at once.
package config

import (
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/pelletier/go-toml/v2"
	"github.com/propheci/botconf/go/describe"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
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
	// Empty means DefaultPluginID.
	PluginID string `toml:"plugin_id" hint:"misc" doc:"The id wuphf-misc's settings have in the core: they live at plugin_settings.<id> and the settings panel shows them as \"<id> (plugin)\". Empty means misc."`
	// 0 means DefaultReconnectDelay.
	ReconnectDelayMs uint32 `toml:"reconnect_delay_ms" hint:"2000" doc:"Pause before resubscribing after the message stream ends, in milliseconds; 0 means 2000."`

	// Runtime holds the settings that may change while wuphf-misc runs.
	// Embedded, so the file keeps its layout (`log_level`, `[instagram]`).
	Runtime
}

// Description is the config file's description, from File's tags.
func Description() *describe.Description {
	return describe.MustOf("wuphf-misc", fileDescription, File{})
}

const fileDescription = "Features that are not bridging. wuphf-misc follows the core's live " +
	"messages through the observer service and replies through it.\n\n" +
	"Runtime settings (log_level and every [instagram] option) can also change while wuphf-misc " +
	"runs: it declares them to the core, which keeps any change (made from its settings panel or " +
	"`wuphf-server settings`, under `plugin_settings.<plugin_id>`) and pushes it back here. The " +
	"values in this file are the base; unsetting a change returns to them. The other options need " +
	"a restart."

// Runtime holds the settings that can change while wuphf-misc runs. It
// declares them to the core (the `botconf` tags, see the shared settings
// package), which stores, validates and pushes every change; the values in
// the file are the base.
type Runtime struct {
	LogLevel  string    `toml:"log_level" botconf:"Log level" example:"info" jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error" doc:"How much wuphf-misc logs (WUPHF_LOG_LEVEL wins). Empty means info."`
	Instagram Instagram `toml:"instagram"`
}

// WithDefaults fills in what an empty setting stands for, so the values
// declared to the core are explicit.
func (r Runtime) WithDefaults() Runtime {
	r.LogLevel = strings.ToLower(r.LogLevel)
	if r.LogLevel == "" {
		r.LogLevel = "info"
	}
	r.Instagram = r.Instagram.WithDefaults()
	return r
}

// Instagram configures the Instagram downloader: a post or reel link posted
// in one of Chats is answered with the post's media. Zero values stand for
// the defaults (see WithDefaults).
type Instagram struct {
	Enabled bool     `toml:"enabled" botconf:"Instagram downloader" example:"false" doc:"Reply to an Instagram post or reel link in one of the chats with the post's video or photos\n\nA carousel's every item. Messages you send count too, from your phone or the TUI; wuphf-misc's own replies never do. Edits do not. Links sent while wuphf-misc is down are not answered.\n\nThe fetch runs on the Instagram connector's logged-in account, through the core's function broker: enable the Instagram connector's [functions] section and the core's matching [[functions.providers]] entry. Every fetch is an Instagram request on that account, so keep the limits low."`
	Chats   []string `toml:"chats" botconf:"Instagram downloader chats" domain:"wuphf.chat" example:"[\"whatsapp-main:120363000000000000@g.us\"]" doc:"The chats whose Instagram links are answered\n\nChat references, as in the core's rules: connector:chat, or connector:chat#thread for one thread only. While none is listed, the downloader answers nowhere."`
	// Nil means true.
	Caption            *bool  `toml:"caption" botconf:"Instagram caption" default:"true" example:"true" doc:"Add \"@author: \" and up to 200 characters of the post's text to the reply."`
	ReplyOnFailure     bool   `toml:"reply_on_failure" botconf:"Instagram failure reply" example:"false" doc:"Reply \"Could not download this post.\" when a fetch fails\n\nOtherwise a failure is only logged."`
	DedupHours         uint32 `toml:"dedup_hours" botconf:"Instagram repeat window (hours)" default:"24" example:"24" doc:"The same post in the same chat is answered once in this many hours."`
	MaxPerMinute       uint32 `toml:"max_per_minute" botconf:"Instagram fetches a minute" default:"10" example:"10" doc:"At most this many fetches a minute, over every chat\n\nMore are skipped and logged."`
	MaxConcurrent      uint32 `toml:"max_concurrent" botconf:"Instagram fetches at once" default:"2" example:"2" doc:"At most this many fetches at a time."`
	CallTimeoutSeconds uint32 `toml:"call_timeout_seconds" botconf:"Instagram fetch timeout (seconds)" default:"60" example:"60" doc:"One fetch's time limit, in seconds\n\nWaiting for the Instagram connector included."`
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

// DefaultPluginID is wuphf-misc's plugin id when plugin_id is unset.
const DefaultPluginID = "misc"

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

// Validate checks what wuphf-misc needs to start. An enabled feature with no
// chats is not an error: the chats can be added from the settings panel.
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

// Plugin is wuphf-misc's plugin id in the core.
func (c File) Plugin() string {
	if c.PluginID == "" {
		return DefaultPluginID
	}
	return c.PluginID
}
