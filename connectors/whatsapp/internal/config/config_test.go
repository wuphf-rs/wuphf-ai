package config

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/settings"
	"github.com/akshettrj/wuphf/connectors/shared/setup"
	"github.com/pelletier/go-toml/v2"
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
	meta := Runtime{Stickers: StickerMetadata{PackName: "custom", AuthorName: "me"}}.StickerMetadata()
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
	if got := (Runtime{MediaConvertTimeoutMs: 1500}).MediaConvertTimeout(); got != 1500*time.Millisecond {
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

// The runtime settings are embedded, so the file keeps its layout.
func TestRuntimeSettingsKeepTheFileLayout(t *testing.T) {
	t.Parallel()

	var cfg File
	err := toml.Unmarshal([]byte(`
connector_id = "whatsapp-main"
log_level = "debug"

[sticker_metadata]
pack_name = "pack"

[rendering]
max_message_length = 100
`), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.ConnectorID != "whatsapp-main" || cfg.Runtime.LogLevel != "debug" ||
		cfg.Runtime.Stickers.PackName != "pack" || cfg.Runtime.Rendering.MaxMessageLength != 100 {
		t.Fatalf("decoded %+v", cfg)
	}
}

func TestExampleConfigLoads(t *testing.T) {
	t.Parallel()

	if _, err := Load("../../config.example.toml"); err != nil {
		t.Fatalf("Load(config.example.toml) error = %v", err)
	}
}

func TestRuntimeDefaultsAreExplicit(t *testing.T) {
	t.Parallel()

	got := Runtime{}.WithDefaults()
	if got.LogLevel != "info" || got.Stickers != (Runtime{}).StickerMetadata() {
		t.Fatalf("WithDefaults() = %+v", got)
	}
}

func TestSupportsDisappearingTimersParsesAndDefaultsOff(t *testing.T) {
	t.Parallel()

	var unset File
	if err := toml.Unmarshal([]byte(`connector_id = "whatsapp-main"`), &unset); err != nil {
		t.Fatal(err)
	}
	if unset.SupportsDisappearingTimers {
		t.Fatal("supports_disappearing_timers should default to false")
	}
	var set File
	if err := toml.Unmarshal([]byte(`supports_disappearing_timers = true`), &set); err != nil {
		t.Fatal(err)
	}
	if !set.SupportsDisappearingTimers {
		t.Fatal("supports_disappearing_timers = true did not parse")
	}
}

func TestResolvedLoginMethodDefaultsFromThePhoneNumber(t *testing.T) {
	t.Parallel()
	if got := (File{}).ResolvedLoginMethod(); got != LoginMethodQR {
		t.Fatalf("ResolvedLoginMethod() = %q, want %q", got, LoginMethodQR)
	}
	if got := (File{PhoneNumber: "15551234567"}).ResolvedLoginMethod(); got != LoginMethodPairCode {
		t.Fatalf("ResolvedLoginMethod() = %q, want %q", got, LoginMethodPairCode)
	}
	if got := (File{PhoneNumber: "15551234567", LoginMethod: LoginMethodQR}).ResolvedLoginMethod(); got != LoginMethodQR {
		t.Fatalf("ResolvedLoginMethod() = %q, want %q", got, LoginMethodQR)
	}
}

func TestValidateChecksTheLoginMethod(t *testing.T) {
	t.Parallel()
	base := File{
		CoreAddress:     "127.0.0.1:50051",
		ConnectorID:     "whatsapp-main",
		CoreTokenEnv:    "WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN",
		SessionDatabase: "file:wa.db",
	}
	if err := base.Validate(); err != nil {
		t.Fatalf("Validate() = %v", err)
	}
	unknown := base
	unknown.LoginMethod = "sms"
	if err := unknown.Validate(); err == nil || !strings.Contains(err.Error(), "login_method") {
		t.Fatalf("Validate() = %v, want a login_method error", err)
	}
	noPhone := base
	noPhone.LoginMethod = LoginMethodPairCode
	if err := noPhone.Validate(); err == nil || !strings.Contains(err.Error(), "needs phone_number") {
		t.Fatalf("Validate() = %v, want a phone_number error", err)
	}
}

// The runtime settings declaration is shared with the Rust connector, which
// declares the same settings and checks its declaration against this file.
// WUPHF_UPDATE_FIXTURES=1 regenerates it; otherwise this checks that the Go
// connector still declares it.
func TestRuntimeDeclarationMatchesTheSharedFixture(t *testing.T) {
	t.Parallel()
	declaration, err := settings.Declare(Runtime{}.WithDefaults())
	if err != nil {
		t.Fatal(err)
	}
	got, err := json.MarshalIndent(struct {
		DescriptorHex string          `json:"descriptor_hex"`
		JSONSchema    json.RawMessage `json:"json_schema"`
		Base          json.RawMessage `json:"base"`
	}{
		DescriptorHex: hex.EncodeToString(declaration.GetDescriptor_()),
		JSONSchema:    json.RawMessage(declaration.GetJsonSchema()),
		Base:          json.RawMessage(declaration.GetBaseJson()),
	}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	got = append(got, '\n')
	path := filepath.Join("testdata", "runtime-declaration.json")
	if os.Getenv("WUPHF_UPDATE_FIXTURES") == "1" {
		if err := os.MkdirAll("testdata", 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, got, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("%s differs from what this connector declares (regenerate with WUPHF_UPDATE_FIXTURES=1)", path)
	}
}

// The checked-in example is the description's: WUPHF_WRITE_EXAMPLES=1 go
// test ./... rewrites it.
func TestTheExampleIsGeneratedFromTheConfigType(t *testing.T) {
	if err := setup.CheckExample(Description(), "../../config.example.toml"); err != nil {
		t.Fatal(err)
	}
}

func TestTheSessionFileIsTheDatabaseURIsPath(t *testing.T) {
	for database, want := range map[string]string{
		"file:whatsapp.session.db?_foreign_keys=on": "whatsapp.session.db",
		"file:///var/lib/wuphf/wa.db":               "/var/lib/wuphf/wa.db",
		"whatsapp.session.db":                       "whatsapp.session.db",
	} {
		if got := (File{SessionDatabase: database}).SessionFile(); got != want {
			t.Errorf("%s: %q, want %q", database, got, want)
		}
	}
}
