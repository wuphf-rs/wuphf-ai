package config

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/setup"
)

func TestLoadAcceptsTheExample(t *testing.T) {
	t.Parallel()

	cfg, err := Load(filepath.Join("..", "..", "config.example.toml"))
	if err != nil {
		t.Fatalf("Load(config.example.toml) error = %v", err)
	}
	if cfg.ProviderID != "mediaconv" || cfg.TokenEnv != "WUPHF_PROVIDER_MEDIACONV_TOKEN" {
		t.Fatalf("cfg = %+v", cfg)
	}
	if cfg.Concurrency() != uint32(runtime.NumCPU()) || cfg.DefaultDeadline() != 0 || cfg.ReconnectDelay() != 0 {
		t.Fatalf("defaults = %d, %v, %v", cfg.Concurrency(), cfg.DefaultDeadline(), cfg.ReconnectDelay())
	}
}

func TestLoadAppliesExplicitValues(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "mediaconv.toml")
	if err := os.WriteFile(path, []byte(`
core_address = "127.0.0.1:50051"
provider_id = "mediaconv"
token_env = "TOKEN"
max_concurrency = 3
default_deadline_ms = 15000
reconnect_delay_ms = 500
`), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Concurrency() != 3 || cfg.DefaultDeadline() != 15*time.Second || cfg.ReconnectDelay() != 500*time.Millisecond {
		t.Fatalf("cfg = %+v", cfg)
	}
}

func TestValidateRequiresEachKey(t *testing.T) {
	t.Parallel()

	valid := File{CoreAddress: "127.0.0.1:50051", ProviderID: "mediaconv", TokenEnv: "TOKEN"}
	if err := valid.Validate(); err != nil {
		t.Fatalf("Validate() error = %v for a valid file", err)
	}
	for name, cfg := range map[string]File{
		"core_address": {ProviderID: "mediaconv", TokenEnv: "TOKEN"},
		"provider_id":  {CoreAddress: "127.0.0.1:50051", TokenEnv: "TOKEN"},
		"token_env":    {CoreAddress: "127.0.0.1:50051", ProviderID: "mediaconv"},
	} {
		if err := cfg.Validate(); err == nil {
			t.Errorf("Validate() succeeded without %s", name)
		}
	}
}

// The checked-in example is the description's: WUPHF_WRITE_EXAMPLES=1 go
// test ./... rewrites it.
func TestTheExampleIsGeneratedFromTheConfigType(t *testing.T) {
	if err := setup.CheckExample(Description(), "../../config.example.toml"); err != nil {
		t.Fatal(err)
	}
}
