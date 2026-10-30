package config

import (
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress string `toml:"core_address"`
	ConnectorID string `toml:"connector_id"`
	// CoreTokenEnv names the environment variable holding this connector's core
	// token; it must match the core's [[connectors]].token_env for this
	// connector id. Required.
	CoreTokenEnv string `toml:"core_token_env"`
	// CookiesFile is the JSON map of Instagram session cookies used to
	// authenticate on first start (e.g. exported from a logged-in browser).
	CookiesFile string `toml:"cookies_file"`
	// SessionFile is where the connector persists the latest cookie state on
	// shutdown and reloads it on the next start, so refreshed cookies survive a
	// restart without re-exporting from the browser. Falls back to CookiesFile
	// when it does not yet exist.
	SessionFile string `toml:"session_file"`
	// StateFile is where the connector records its position in Instagram's
	// delta stream, so a restart replays what arrived while it was down.
	// Optional; defaults to DefaultStateFile.
	StateFile string `toml:"state_file"`
	// Instagram has no topics; these stay false but are surfaced so the core's
	// capability gate is configured from one place.
	SupportsThreads     bool `toml:"supports_threads"`
	SupportsTopicCreate bool `toml:"supports_topic_create"`
	// Runtime holds the settings that may change while the connector runs.
	// Embedded, so the file keeps its layout (`log_level`).
	Runtime
	// BackfillOnStart resumes Instagram's delta stream from the position in
	// StateFile, so messages that arrived while the connector was down are
	// bridged, the same offline-sync behaviour WhatsApp gets from history
	// sync. The core dedups by native id (persisted), so anything already
	// bridged is dropped. Nil means enabled; set it to false to ignore the saved
	// position and bridge only what arrives from now on.
	BackfillOnStart *bool `toml:"backfill_on_start"`
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
	LogLevel string `toml:"log_level" botconf:"Log level" doc:"How much the connector logs (WUPHF_LOG_LEVEL wins)" jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error"`
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
	return nil
}
