package config

import (
	"testing"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/setup"
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
	if err := (File{CoreAddress: "a", ObserverTokenEnv: "T", Instagram: Instagram{Enabled: true}}).Validate(); err == nil {
		t.Fatal("Validate() accepted an enabled feature without chats")
	}
}

func TestValidateRequiresCoreAndObserverToken(t *testing.T) {
	t.Parallel()
	for _, cfg := range []File{
		{ObserverTokenEnv: "T"},
		{CoreAddress: "127.0.0.1:50051"},
		{CoreAddress: "127.0.0.1:50051", ObserverTokenEnv: "T", LogLevel: "loud"},
	} {
		if err := cfg.Validate(); err == nil {
			t.Errorf("Validate(%+v) accepted it", cfg)
		}
	}
}
