package config

import (
	"testing"
	"time"

	"github.com/pelletier/go-toml/v2"
)

func TestValidateRequiresCoreConnectorAndSession(t *testing.T) {
	t.Parallel()

	cfg := File{ConnectorID: "telegram-main", SessionFile: "./telegram.session.json"}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() succeeded without core_address")
	}
}

func TestValidateRejectsTopicCreateWithoutThreads(t *testing.T) {
	t.Parallel()

	cfg := File{
		CoreAddress:         "127.0.0.1:50051",
		ConnectorID:         "telegram-main",
		SessionFile:         "./telegram.session.json",
		SupportsTopicCreate: true,
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() succeeded for supports_topic_create without supports_threads")
	}
}

func TestValidateAcceptsThreadedConnector(t *testing.T) {
	t.Parallel()

	cfg := File{
		CoreAddress:         "127.0.0.1:50051",
		ConnectorID:         "telegram-main",
		CoreTokenEnv:        "WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN",
		SessionFile:         "./telegram.session.json",
		SupportsThreads:     true,
		SupportsTopicCreate: true,
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() failed for valid threaded connector: %v", err)
	}
}

func TestMediaConvertTimeoutDefaultsAndOverrides(t *testing.T) {
	t.Parallel()

	if got := (Runtime{}).MediaConvertTimeout(); got != 20*time.Second {
		t.Errorf("default MediaConvertTimeout() = %v, want 20s", got)
	}
	if got := (Runtime{MediaConvertTimeoutMs: 1500}).MediaConvertTimeout(); got != 1500*time.Millisecond {
		t.Errorf("MediaConvertTimeout() = %v, want 1.5s", got)
	}
}

func TestValidateRequiresCoreTokenEnv(t *testing.T) {
	t.Parallel()

	cfg := File{
		CoreAddress: "127.0.0.1:50051",
		ConnectorID: "telegram-main",
		SessionFile: "./telegram.session.json",
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() succeeded without core_token_env")
	}
}

// The runtime settings are embedded, so the file keeps its layout.
func TestRuntimeSettingsKeepTheFileLayout(t *testing.T) {
	t.Parallel()

	var cfg File
	err := toml.Unmarshal([]byte(`
connector_id = "telegram-main"
log_level = "debug"
media_convert_timeout_ms = 1500

[rendering]
edit_mode = "repost"
max_message_length = 100
`), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.ConnectorID != "telegram-main" || cfg.Runtime.LogLevel != "debug" ||
		cfg.Runtime.MediaConvertTimeoutMs != 1500 || cfg.Runtime.Rendering.EditMode != "repost" ||
		cfg.Runtime.Rendering.MaxMessageLength != 100 {
		t.Fatalf("decoded %+v", cfg)
	}
}

func TestExampleConfigLoads(t *testing.T) {
	t.Parallel()

	cfg, err := Load("../../config.example.toml")
	if err != nil {
		t.Fatalf("Load(config.example.toml) error = %v", err)
	}
	if cfg.Runtime.Rendering.EditMode != EditModeInPlace {
		t.Fatalf("example edit_mode = %q", cfg.Runtime.Rendering.EditMode)
	}
}

func TestRuntimeDefaultsAreExplicit(t *testing.T) {
	t.Parallel()

	got := Runtime{}.WithDefaults()
	if got.LogLevel != "info" || got.Rendering.EditMode != EditModeInPlace || got.Rendering.DeleteMode != DeleteModeRevoke {
		t.Fatalf("WithDefaults() = %+v", got)
	}
}
