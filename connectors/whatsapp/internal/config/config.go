package config

import (
	"errors"
	"fmt"
	"os"

	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress     string `toml:"core_address"`
	ConnectorID     string `toml:"connector_id"`
	SessionDatabase string `toml:"session_database"`
	// WhatsApp has no topics; these stay false but are surfaced so the core's
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
	if c.CoreAddress == "" || c.ConnectorID == "" || c.SessionDatabase == "" {
		return errors.New("core_address, connector_id, and session_database are required")
	}
	if c.SupportsTopicCreate && !c.SupportsThreads {
		return errors.New("supports_topic_create requires supports_threads")
	}
	return nil
}
