package config

import (
	"testing"
	"time"
)

func TestStickerMetadataFillsDefaults(t *testing.T) {
	t.Parallel()
	meta := File{}.StickerMetadata()
	if meta.PackName != "wuphf" || meta.AuthorName != "wuphf-ai" || meta.Link != "https://github.com/wuphf/wuphf-ai" {
		t.Fatalf("defaults = %#v", meta)
	}
}

func TestStickerMetadataKeepsConfigured(t *testing.T) {
	t.Parallel()
	meta := File{Stickers: StickerMetadata{PackName: "custom", AuthorName: "me"}}.StickerMetadata()
	if meta.PackName != "custom" || meta.AuthorName != "me" {
		t.Fatalf("configured fields overridden: %#v", meta)
	}
	if meta.Link != "https://github.com/wuphf/wuphf-ai" {
		t.Fatalf("blank link should fall back to default, got %q", meta.Link)
	}
}

func TestResolvedClientModeDefaultsToAndroid(t *testing.T) {
	t.Parallel()
	if got := (File{}).ResolvedClientMode(); got != ClientModeAndroid {
		t.Fatalf("ResolvedClientMode() = %q, want %q", got, ClientModeAndroid)
	}
	if got := (File{ClientMode: ClientModeAndroidBusiness}).ResolvedClientMode(); got != ClientModeAndroidBusiness {
		t.Fatalf("ResolvedClientMode() = %q, want %q", got, ClientModeAndroidBusiness)
	}
}

func TestValidateRejectsUnknownClientMode(t *testing.T) {
	t.Parallel()
	cfg := File{
		CoreAddress:     "127.0.0.1:50051",
		ConnectorID:     "whatsapp-main",
		SessionDatabase: "file:wa.db",
		ClientMode:      "iphone",
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted an unknown client_mode")
	}
}

func TestValidateAcceptsBusinessClientMode(t *testing.T) {
	t.Parallel()
	cfg := File{
		CoreAddress:     "127.0.0.1:50051",
		ConnectorID:     "whatsapp-main",
		CoreTokenEnv:    "WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN",
		SessionDatabase: "file:wa.db",
		ClientMode:      ClientModeAndroidBusiness,
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() rejected business client_mode: %v", err)
	}
}

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
		CoreTokenEnv:    "WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN",
		SessionDatabase: "file:session.db?_foreign_keys=on",
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() error = %v", err)
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
		CoreAddress:     "127.0.0.1:50051",
		ConnectorID:     "whatsapp-main",
		SessionDatabase: "file:session.db?_foreign_keys=on",
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted configuration without core_token_env")
	}
}
