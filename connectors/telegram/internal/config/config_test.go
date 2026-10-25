package config

import (
	"testing"
	"time"
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

	if got := (File{}).MediaConvertTimeout(); got != 20*time.Second {
		t.Errorf("default MediaConvertTimeout() = %v, want 20s", got)
	}
	if got := (File{MediaConvertTimeoutMs: 1500}).MediaConvertTimeout(); got != 1500*time.Millisecond {
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
