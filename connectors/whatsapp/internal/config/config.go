package config

import (
	"errors"
	"fmt"
	"os"

	"github.com/pelletier/go-toml/v2"
)

type Target struct {
	EndpointID string `toml:"endpoint_id"`
	GroupID    string `toml:"group_id"`
	ChatJID    string `toml:"chat_jid"`
}

type File struct {
	CoreAddress     string   `toml:"core_address"`
	ConnectorID     string   `toml:"connector_id"`
	SessionDatabase string   `toml:"session_database"`
	Endpoints       []string `toml:"endpoints"`
	Targets         []Target `toml:"targets"`
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
	if len(c.Endpoints) == 0 || len(c.Targets) == 0 {
		return errors.New("at least one endpoint and target are required")
	}
	owned := make(map[string]struct{}, len(c.Endpoints))
	for _, endpoint := range c.Endpoints {
		if endpoint == "" {
			return errors.New("endpoint IDs cannot be empty")
		}
		if _, exists := owned[endpoint]; exists {
			return fmt.Errorf("duplicate endpoint %q", endpoint)
		}
		owned[endpoint] = struct{}{}
	}
	targetEndpoints := make(map[string]struct{}, len(c.Targets))
	chats := make(map[string]string, len(c.Targets))
	for _, target := range c.Targets {
		if target.EndpointID == "" || target.GroupID == "" || target.ChatJID == "" {
			return errors.New("target endpoint_id, group_id, and chat_jid are required")
		}
		if _, exists := owned[target.EndpointID]; !exists {
			return fmt.Errorf("target endpoint %q is not owned by connector", target.EndpointID)
		}
		if _, exists := targetEndpoints[target.EndpointID]; exists {
			return fmt.Errorf("duplicate outbound target for endpoint %q", target.EndpointID)
		}
		targetEndpoints[target.EndpointID] = struct{}{}
		if existing, exists := chats[target.ChatJID]; exists {
			return fmt.Errorf("ambiguous inbound chat %q for endpoints %q and %q", target.ChatJID, existing, target.EndpointID)
		}
		chats[target.ChatJID] = target.EndpointID
	}
	return nil
}
