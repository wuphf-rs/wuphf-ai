package config

import (
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/pelletier/go-toml/v2"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/setup"
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

// The core refuses a declaration whose log level it does not know, so an
// unknown level fails at load rather than at every registration.
func TestValidateRejectsAnUnknownLogLevel(t *testing.T) {
	t.Parallel()

	cfg := File{
		CoreAddress:  "127.0.0.1:50051",
		ConnectorID:  "telegram-main",
		CoreTokenEnv: "TOKEN",
		SessionFile:  "./telegram.session.json",
		Runtime:      Runtime{LogLevel: "verbose"},
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted log_level = verbose")
	}
	cfg.LogLevel = "DEBUG"
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() rejected DEBUG: %v", err)
	}
	if got := cfg.Runtime.WithDefaults().LogLevel; got != "debug" {
		t.Fatalf("WithDefaults() log level = %q, want debug", got)
	}
}

// setTelegramEnv sets the credential variables a bot session needs, leaving
// WUPHF_TELEGRAM_BOT_TOKEN and WUPHF_TELEGRAM_PHONE to the caller.
func setTelegramEnv(t *testing.T, botToken, phone string) {
	t.Helper()
	t.Setenv("WUPHF_TELEGRAM_APP_ID", "12345")
	t.Setenv("WUPHF_TELEGRAM_APP_HASH", "hash")
	t.Setenv("WUPHF_TELEGRAM_BOT_TOKEN", botToken)
	t.Setenv("WUPHF_TELEGRAM_PHONE", phone)
}

func TestBotTokenComesFromTheFileWhenTheEnvironmentHasNone(t *testing.T) {
	setTelegramEnv(t, "", "")

	credentials, err := LoadCredentials(File{BotToken: " 111:file "})
	if err != nil {
		t.Fatalf("LoadCredentials() error = %v", err)
	}
	if credentials.BotToken != "111:file" {
		t.Fatalf("bot token = %q, want the file's", credentials.BotToken)
	}
}

func TestBotTokenFromTheEnvironmentWinsOverTheFile(t *testing.T) {
	setTelegramEnv(t, "222:env", "")

	credentials, err := LoadCredentials(File{BotToken: "111:file"})
	if err != nil {
		t.Fatalf("LoadCredentials() error = %v", err)
	}
	if credentials.BotToken != "222:env" {
		t.Fatalf("bot token = %q, want the environment's", credentials.BotToken)
	}
}

func TestCredentialsNeedABotTokenOrAPhone(t *testing.T) {
	setTelegramEnv(t, "", "")

	if _, err := LoadCredentials(File{}); err == nil {
		t.Fatal("LoadCredentials() accepted neither a bot token nor a phone")
	}
}

func TestBotTokenIsNotARuntimeSetting(t *testing.T) {
	t.Parallel()

	// Runtime settings are declared to the core and pushed back; a secret must
	// never be one of them.
	encoded, err := toml.Marshal(Runtime{})
	if err != nil {
		t.Fatal(err)
	}
	for _, secret := range []string{"bot_token", "panel_bot_token"} {
		if strings.Contains(string(encoded), secret) {
			t.Fatalf("Runtime carries %s:\n%s", secret, encoded)
		}
	}
}

// validFile is the smallest configuration Validate accepts.
func validFile() File {
	return File{
		CoreAddress:  "127.0.0.1:50051",
		ConnectorID:  "telegram-main",
		CoreTokenEnv: "TOKEN",
		SessionFile:  "./telegram.session.json",
	}
}

func TestPanelCommandDefaultsToWuphf(t *testing.T) {
	t.Parallel()

	cfg := validFile()
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() error = %v", err)
	}
	if got := cfg.Panel.CommandName(); got != "wuphf" {
		t.Fatalf("CommandName() = %q, want wuphf", got)
	}
	cfg.Panel.Command = "settings_2"
	if got := cfg.Panel.CommandName(); got != "settings_2" {
		t.Fatalf("CommandName() = %q, want the configured one", got)
	}
}

func TestPanelCommandMustBeLowercaseWord(t *testing.T) {
	t.Parallel()

	for _, command := range []string{"Wuphf", "wu phf", "/wuphf", "wuphf-x", strings.Repeat("a", 33)} {
		cfg := validFile()
		cfg.Panel.Command = command
		if err := cfg.Validate(); err == nil {
			t.Errorf("Validate() accepted panel.command %q", command)
		}
	}
	cfg := validFile()
	cfg.Panel.Command = strings.Repeat("a", 32)
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() rejected a 32-character command: %v", err)
	}
}

func TestPanelSessionFileRequiredWithPanelBot(t *testing.T) {
	setTelegramEnv(t, "", "+10000000000")
	t.Setenv("WUPHF_TELEGRAM_PANEL_BOT_TOKEN", "")

	file := File{PanelBotToken: "333:panel"}
	if _, err := LoadCredentials(file); err == nil {
		t.Fatal("LoadCredentials() accepted a panel bot without panel.session_file")
	}
	file.Panel.SessionFile = "./telegram-panel.session.json"
	credentials, err := LoadCredentials(file)
	if err != nil {
		t.Fatalf("LoadCredentials() error = %v", err)
	}
	if credentials.PanelBotToken != "333:panel" {
		t.Fatalf("panel bot token = %q, want the file's", credentials.PanelBotToken)
	}
}

func TestPanelBotTokenFromTheEnvironmentWins(t *testing.T) {
	setTelegramEnv(t, "", "+10000000000")
	t.Setenv("WUPHF_TELEGRAM_PANEL_BOT_TOKEN", "444:env")

	credentials, err := LoadCredentials(File{PanelBotToken: "333:file", Panel: PanelOptions{SessionFile: "panel.json"}})
	if err != nil {
		t.Fatalf("LoadCredentials() error = %v", err)
	}
	if credentials.PanelBotToken != "444:env" {
		t.Fatalf("panel bot token = %q, want the environment's", credentials.PanelBotToken)
	}
}

// The checked-in example is the description's: WUPHF_WRITE_EXAMPLES=1 go
// test ./... rewrites it.
func TestTheExampleIsGeneratedFromTheConfigType(t *testing.T) {
	if err := setup.CheckExample(Description(), "../../config.example.toml"); err != nil {
		t.Fatal(err)
	}
}

// Every variable the credentials are read from is on setup's Environment
// screen, and nothing else is.
func TestTheEnvironmentScreenListsTheCredentials(t *testing.T) {
	read := map[string]bool{}
	ReadCredentials(File{}, func(name string) string {
		read[name] = true
		return "1"
	})
	listed := map[string]bool{}
	for _, variable := range Description().Env {
		listed[variable.Name] = true
	}
	if !reflect.DeepEqual(read, listed) {
		t.Fatalf("read %v, listed %v", read, listed)
	}
}
