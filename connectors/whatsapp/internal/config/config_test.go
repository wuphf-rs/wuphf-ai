package config

import "testing"

func TestValidateRequiresCoreIdentity(t *testing.T) {
	t.Parallel()
	cfg := File{CoreAddress: "127.0.0.1:50051", ConnectorID: "whatsapp-main"}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted configuration without a session database")
	}
}

func TestValidateRejectsTopicCreateWithoutThreads(t *testing.T) {
	t.Parallel()
	cfg := File{
		CoreAddress:         "127.0.0.1:50051",
		ConnectorID:         "whatsapp-main",
		SessionDatabase:     "file:session.db?_foreign_keys=on",
		SupportsTopicCreate: true,
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted supports_topic_create without supports_threads")
	}
}

func TestValidateAcceptsMinimalConfiguration(t *testing.T) {
	t.Parallel()
	cfg := File{
		CoreAddress:     "127.0.0.1:50051",
		ConnectorID:     "whatsapp-main",
		SessionDatabase: "file:session.db?_foreign_keys=on",
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() error = %v", err)
	}
}
