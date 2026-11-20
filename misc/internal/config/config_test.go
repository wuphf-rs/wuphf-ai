package config

import "testing"

func TestExampleConfigLoads(t *testing.T) {
	t.Parallel()
	cfg, err := Load("../../config.example.toml")
	if err != nil {
		t.Fatalf("Load(config.example.toml) error = %v", err)
	}
	if cfg.ObserverTokenEnv != "WUPHF_OBSERVER_TOKEN" || cfg.ReconnectDelay() != DefaultReconnectDelay {
		t.Fatalf("config = %+v", cfg)
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
