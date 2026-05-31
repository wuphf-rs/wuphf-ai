package config

import "testing"

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
		SessionFile:         "./telegram.session.json",
		SupportsThreads:     true,
		SupportsTopicCreate: true,
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() failed for valid threaded connector: %v", err)
	}
}
