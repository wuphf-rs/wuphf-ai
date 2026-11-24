package config

import (
	"strings"
	"testing"
	"time"

	"github.com/wuphf-rs/wuphf-ai/connectors/shared/setup"
)

// The checked-in example is the description's: WUPHF_WRITE_EXAMPLES=1 go
// test ./... rewrites it.
func TestTheExampleIsGeneratedFromTheConfigType(t *testing.T) {
	if err := setup.CheckExample(Description(), "../../config.example.toml"); err != nil {
		t.Fatal(err)
	}
}

func TestExampleConfigLoads(t *testing.T) {
	t.Parallel()
	cfg, err := Load("../../config.example.toml")
	if err != nil {
		t.Fatalf("Load(config.example.toml) error = %v", err)
	}
	if cfg.ObserverTokenEnv != "WUPHF_OBSERVER_TOKEN" || cfg.ReconnectDelay() != DefaultReconnectDelay {
		t.Fatalf("config = %+v", cfg)
	}
	instagram := cfg.Instagram.WithDefaults()
	if instagram.Enabled || len(instagram.Chats) != 1 || !*instagram.Caption || instagram.MaxPerMinute != 10 || instagram.CallTimeout() != time.Minute {
		t.Fatalf("instagram = %+v", instagram)
	}
}

func TestInstagramDefaults(t *testing.T) {
	t.Parallel()
	got := Instagram{}.WithDefaults()
	if !*got.Caption || got.Dedup() != 24*time.Hour || got.MaxPerMinute != 10 || got.MaxConcurrent != 2 || got.CallTimeout() != time.Minute {
		t.Fatalf("defaults = %+v", got)
	}
	// The chats can come from the settings panel.
	if err := (File{CoreAddress: "a", ObserverTokenEnv: "T", Runtime: Runtime{Instagram: Instagram{Enabled: true}}}).Validate(); err != nil {
		t.Fatalf("Validate() refused an enabled feature without chats: %v", err)
	}
}

func TestValidateRequiresCoreAndObserverToken(t *testing.T) {
	t.Parallel()
	for _, cfg := range []File{
		{ObserverTokenEnv: "T"},
		{CoreAddress: "127.0.0.1:50051"},
		{CoreAddress: "127.0.0.1:50051", ObserverTokenEnv: "T", Runtime: Runtime{LogLevel: "loud"}},
	} {
		if err := cfg.Validate(); err == nil {
			t.Errorf("Validate(%+v) accepted it", cfg)
		}
	}
}

// [instagram] chats are picked by name in setup: they name the chat domain.
func TestInstagramChatsNameTheChatDomain(t *testing.T) {
	for _, entry := range Description().Table.Entries {
		if entry.Key != "instagram" {
			continue
		}
		for _, field := range entry.Node.Table.Entries {
			if field.Key == "chats" && field.Domain == setup.ChatDomain {
				return
			}
		}
	}
	t.Fatal("instagram.chats has no chat domain")
}

func TestRuntimeDefaultsAndPluginID(t *testing.T) {
	t.Parallel()
	got := Runtime{LogLevel: "DEBUG"}.WithDefaults()
	if got.LogLevel != "debug" || !*got.Instagram.Caption || got.Instagram.MaxConcurrent != 2 {
		t.Fatalf("defaults = %+v", got)
	}
	if got := (Runtime{}).WithDefaults().LogLevel; got != "info" {
		t.Fatalf("empty log level = %q", got)
	}
	if (File{}).Plugin() != "misc" || (File{PluginID: "misc2"}).Plugin() != "misc2" {
		t.Fatal("plugin id")
	}
}

// Every [instagram] option and the log level change at runtime; the
// connection options do not.
func TestRuntimeSettingsAreTheFeaturesAndTheLogLevel(t *testing.T) {
	var keys []string
	for _, setting := range Description().RuntimeSettings() {
		keys = append(keys, setting.GetKey())
	}
	want := []string{
		"log_level", "instagram.enabled", "instagram.chats", "instagram.caption",
		"instagram.reply_on_failure", "instagram.dedup_hours", "instagram.max_per_minute",
		"instagram.max_concurrent", "instagram.call_timeout_seconds",
	}
	if strings.Join(keys, " ") != strings.Join(want, " ") {
		t.Fatalf("runtime settings = %v, want %v", keys, want)
	}
}
