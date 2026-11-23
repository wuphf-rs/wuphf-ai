package config

import (
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/pelletier/go-toml/v2"
	"github.com/propheci/botconf/go/describe"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
)

// File is the connector's config file. Each option is described once, in its
// tags (see the shared setup package); config.example.toml is generated from
// them.
type File struct {
	CoreAddress string `toml:"core_address" required:"true" example:"127.0.0.1:50051" doc:"The core's gRPC address."`
	ConnectorID string `toml:"connector_id" required:"true" example:"instagram-main" doc:"Must match a [[connectors]] entry in the core's configuration."`
	// CoreTokenEnv names the variable; the token itself is read from the
	// environment at startup.
	CoreTokenEnv string `toml:"core_token_env" required:"true" env:"token" example:"WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN" doc:"Environment variable holding this connector's core token; must match the core's [[connectors]].token_env for this connector id."`
	CookiesFile  string `toml:"cookies_file" required:"true" example:"instagram.cookies.json" doc:"JSON map of Instagram session cookies, exported from a logged-in browser session, e.g. {\"sessionid\": \"...\", \"csrftoken\": \"...\", \"ds_user_id\": \"...\", \"mid\": \"...\", \"ig_did\": \"...\"}. Used only until a session file is written."`
	SessionFile  string `toml:"session_file" required:"true" example:"instagram.session.json" doc:"Where the connector stores the latest (refreshed) cookies on shutdown and reloads them on the next start, so a restart does not need a fresh export. Falls back to cookies_file when it does not exist yet."`
	// Empty means DefaultStateFile.
	StateFile string `toml:"state_file" default:"instagram.state.json" example:"instagram.state.json" doc:"Where the connector records its position in Instagram's delta stream, so a restart replays what arrived while it was down."`
	// Instagram has no topics; these stay false but are surfaced so the core's
	// capability gate is configured from one place.
	SupportsThreads     bool `toml:"supports_threads" doc:"Instagram has no threads: leave false. The core may only use this connector as a single target or an identity source, never as a threaded mirror."`
	SupportsTopicCreate bool `toml:"supports_topic_create" doc:"Instagram has no threads: leave false."`
	// Runtime holds the settings that may change while the connector runs.
	// Embedded, so the file keeps its layout (`log_level`).
	Runtime
	// Nil means enabled.
	BackfillOnStart *bool `toml:"backfill_on_start" default:"true" example:"true" doc:"Resume the delta stream from the position in state_file, so messages exchanged while the connector was down are bridged (the Instagram counterpart to WhatsApp's offline history sync). The core dedups by native id, so nothing already bridged is repeated. If the saved position is too old, Instagram forces a fresh start and that gap is not replayed. Set it to false to bridge only what arrives from now on."`
	// Nil serves no function.
	Functions *Functions `toml:"functions" doc:"Serve instagram.fetch_media@1 (download a post's or reel's media by link, used by wuphf-misc) through the core's function broker, on this connector's logged-in session. Needs a matching [[functions.providers]] entry in the core's config. Every fetch is an Instagram request on this account."`
}

// Functions configures the provider session the connector opens next to its
// connector session.
type Functions struct {
	ProviderID string `toml:"provider_id" required:"true" hint:"instagram-main" doc:"The core's [[functions.providers]] id for this connector."`
	TokenEnv   string `toml:"token_env" required:"true" env:"token" hint:"WUPHF_PROVIDER_INSTAGRAM_TOKEN" doc:"Environment variable holding the provider token; must match that provider entry's token_env."`
	// 0 means DefaultFetchConcurrency.
	MaxConcurrency uint32 `toml:"max_concurrency" default:"2" doc:"Caps the fetches that run at once."`
}

// Description is the config file's description, from File's tags.
func Description() *describe.Description {
	return describe.MustOf("wuphf Instagram connector", fileDescription, File{})
}

const fileDescription = "Runtime settings (log_level) can also change while the connector runs: " +
	"the connector declares them to the core, which keeps any change (made from its settings panel " +
	"or `wuphf-server settings`, under `connector_settings.<connector id>`) and pushes it back here. " +
	"The values in this file are the base; unsetting a change returns to them.\n\n" +
	"Routing is owned entirely by the core: configure [rules.<id>] entries there, using Instagram " +
	"thread keys (decimal numbers) as chat ids."

// DefaultFetchConcurrency is the fetch concurrency when max_concurrency is unset.
const DefaultFetchConcurrency = 2

// Concurrency returns the configured fetch concurrency, or the default.
func (f Functions) Concurrency() uint32 {
	if f.MaxConcurrency == 0 {
		return DefaultFetchConcurrency
	}
	return f.MaxConcurrency
}

// DefaultStateFile is the sync state path used when state_file is unset.
const DefaultStateFile = "instagram.state.json"

// StatePath returns the configured sync state path, or the default.
func (c File) StatePath() string {
	if c.StateFile == "" {
		return DefaultStateFile
	}
	return c.StateFile
}

// Runtime holds the settings that can change while the connector runs. The
// connector declares them to the core (the `botconf` tags, see the shared
// settings package), which stores, validates and pushes every change; the
// values in the file are the base. Everything else in File is read once
// (`backfill_on_start` too: it is read when the session connects).
type Runtime struct {
	// LogLevel sets log verbosity (trace, debug, info, warn, error). Empty
	// means info; the WUPHF_LOG_LEVEL environment variable overrides it.
	LogLevel string `toml:"log_level" botconf:"Log level" example:"info" doc:"How much the connector logs (WUPHF_LOG_LEVEL wins)\n\nAny case; anything else is refused. Empty means info." jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error"`
}

// WithDefaults fills in what an empty setting stands for, so the values
// declared to the core are explicit.
func (c Runtime) WithDefaults() Runtime {
	c.LogLevel = strings.ToLower(c.LogLevel)
	if c.LogLevel == "" {
		c.LogLevel = "info"
	}
	return c
}

// ShouldBackfill reports whether the connector resumes from its saved stream
// position, defaulting to true when the option is unset.
func (c File) ShouldBackfill() bool {
	return c.BackfillOnStart == nil || *c.BackfillOnStart
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

func (c File) Validate() error {
	if err := logging.CheckLevel(c.LogLevel); err != nil {
		return err
	}
	if c.CoreAddress == "" || c.ConnectorID == "" || c.CoreTokenEnv == "" || c.CookiesFile == "" || c.SessionFile == "" {
		return errors.New("core_address, connector_id, core_token_env, cookies_file, and session_file are required")
	}
	if c.SupportsTopicCreate && !c.SupportsThreads {
		return errors.New("supports_topic_create requires supports_threads")
	}
	if c.Functions != nil && (c.Functions.ProviderID == "" || c.Functions.TokenEnv == "") {
		return errors.New("[functions] needs provider_id and token_env")
	}
	return nil
}
