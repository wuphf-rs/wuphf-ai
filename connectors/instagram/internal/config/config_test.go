package config

import (
	"testing"

	"github.com/akshettrj/wuphf/connectors/shared/setup"
	"github.com/pelletier/go-toml/v2"
)

// The checked-in example is the description's: WUPHF_WRITE_EXAMPLES=1 go
// test ./... rewrites it.
func TestTheExampleIsGeneratedFromTheConfigType(t *testing.T) {
	if err := setup.CheckExample(Description(), "../../config.example.toml"); err != nil {
		t.Fatal(err)
	}
}

func TestShouldBackfillDefaultsToEnabled(t *testing.T) {
	t.Parallel()
	enabled := true
	disabled := false
	cases := []struct {
		name string
		cfg  File
		want bool
	}{
		{"unset defaults to enabled", File{}, true},
		{"explicitly enabled", File{BackfillOnStart: &enabled}, true},
		{"explicitly disabled", File{BackfillOnStart: &disabled}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := tc.cfg.ShouldBackfill(); got != tc.want {
				t.Fatalf("ShouldBackfill() = %v, want %v", got, tc.want)
			}
		})
	}
}

func TestValidateRequiresCoreTokenEnv(t *testing.T) {
	t.Parallel()
	cfg := File{
		CoreAddress:  "127.0.0.1:50051",
		ConnectorID:  "instagram-main",
		CookiesFile:  "./cookies.json",
		SessionFile:  "./instagram.session.json",
		CoreTokenEnv: "WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN",
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() error = %v", err)
	}
	cfg.CoreTokenEnv = ""
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted configuration without core_token_env")
	}
}

// The runtime settings are embedded, so the file keeps its layout.
func TestRuntimeSettingsKeepTheFileLayout(t *testing.T) {
	t.Parallel()

	var cfg File
	if err := toml.Unmarshal([]byte("connector_id = \"instagram-main\"\nlog_level = \"debug\"\n"), &cfg); err != nil {
		t.Fatal(err)
	}
	if cfg.ConnectorID != "instagram-main" || cfg.Runtime.LogLevel != "debug" {
		t.Fatalf("decoded %+v", cfg)
	}
	if got := (Runtime{}).WithDefaults().LogLevel; got != "info" {
		t.Fatalf("default log level = %q", got)
	}
}

func TestExampleConfigLoads(t *testing.T) {
	t.Parallel()

	if _, err := Load("../../config.example.toml"); err != nil {
		t.Fatalf("Load(config.example.toml) error = %v", err)
	}
}

func TestFunctionsSection(t *testing.T) {
	t.Parallel()
	data := `core_address = "127.0.0.1:50051"
connector_id = "instagram-main"
core_token_env = "TOKEN"
cookies_file = "c.json"
session_file = "s.json"

[functions]
provider_id = "instagram-main"
token_env = "WUPHF_PROVIDER_INSTAGRAM_TOKEN"
`
	var cfg File
	if err := toml.Unmarshal([]byte(data), &cfg); err != nil {
		t.Fatal(err)
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("Validate() error = %v", err)
	}
	if cfg.Functions == nil || cfg.Functions.ProviderID != "instagram-main" || cfg.Functions.Concurrency() != DefaultFetchConcurrency {
		t.Fatalf("functions = %+v", cfg.Functions)
	}
	cfg.Functions.TokenEnv = ""
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted [functions] without token_env")
	}
	if (Functions{MaxConcurrency: 5}).Concurrency() != 5 {
		t.Fatal("max_concurrency ignored")
	}
}
