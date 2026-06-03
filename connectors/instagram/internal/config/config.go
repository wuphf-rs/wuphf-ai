package config

import (
	"errors"
	"fmt"
	"os"

	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress string `toml:"core_address"`
	ConnectorID string `toml:"connector_id"`
	// CookiesFile is the JSON map of Instagram session cookies used to
	// authenticate on first start (e.g. exported from a logged-in browser).
	CookiesFile string `toml:"cookies_file"`
	// SessionFile is where the connector persists the latest cookie state on
	// shutdown and reloads it on the next start, so refreshed cookies survive a
	// restart without re-exporting from the browser. Falls back to CookiesFile
	// when it does not yet exist.
	SessionFile string `toml:"session_file"`
	// Instagram has no topics; these stay false but are surfaced so the core's
	// capability gate is configured from one place.
	SupportsThreads     bool `toml:"supports_threads"`
	SupportsTopicCreate bool `toml:"supports_topic_create"`
}

func Load(path string) (File, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return File{}, fmt.Errorf("read configuration: %w", err)
	}
	var cfg File
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return File{}, fmt.Errorf("decode configuration: %w", err)
	}
	if err := cfg.Validate(); err != nil {
		return File{}, err
	}
	return cfg, nil
}

func (c File) Validate() error {
	if c.CoreAddress == "" || c.ConnectorID == "" || c.CookiesFile == "" || c.SessionFile == "" {
		return errors.New("core_address, connector_id, cookies_file, and session_file are required")
	}
	if c.SupportsTopicCreate && !c.SupportsThreads {
		return errors.New("supports_topic_create requires supports_threads")
	}
	return nil
}
