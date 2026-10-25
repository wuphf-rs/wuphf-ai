package config

import "testing"

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
