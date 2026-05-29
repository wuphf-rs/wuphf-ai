package config

import (
	"errors"
	"fmt"
	"os"
	"strconv"

	"github.com/pelletier/go-toml/v2"
)

type Target struct {
	EndpointID         string `toml:"endpoint_id"`
	GroupID            string `toml:"group_id"`
	Peer               string `toml:"peer"`
	IncomingPeerType   string `toml:"incoming_peer_type"`
	IncomingPeerID     int64  `toml:"incoming_peer_id"`
	TopicRootMessageID int    `toml:"topic_root_message_id"`
}

type PeerKey struct {
	Type string
	ID   int64
}

func (t Target) PeerKey() PeerKey {
	return PeerKey{Type: t.IncomingPeerType, ID: t.IncomingPeerID}
}

type File struct {
	CoreAddress string   `toml:"core_address"`
	ConnectorID string   `toml:"connector_id"`
	SessionFile string   `toml:"session_file"`
	Endpoints   []string `toml:"endpoints"`
	Targets     []Target `toml:"targets"`
}

type Credentials struct {
	AppID    int
	AppHash  string
	Phone    string
	Password string
	BotToken string
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
	if c.CoreAddress == "" || c.ConnectorID == "" || c.SessionFile == "" {
		return errors.New("core_address, connector_id, and session_file are required")
	}
	if len(c.Endpoints) == 0 {
		return errors.New("at least one endpoint is required")
	}
	owned := make(map[string]struct{}, len(c.Endpoints))
	for _, endpoint := range c.Endpoints {
		if endpoint == "" {
			return errors.New("endpoint IDs cannot be empty")
		}
		if _, ok := owned[endpoint]; ok {
			return fmt.Errorf("duplicate endpoint %q", endpoint)
		}
		owned[endpoint] = struct{}{}
	}
	if len(c.Targets) == 0 {
		return errors.New("at least one Telegram target is required")
	}
	outbound := make(map[string]struct{}, len(c.Targets))
	inbound := make(map[PeerKey]map[int]string, len(c.Targets))
	for _, target := range c.Targets {
		if target.EndpointID == "" || target.GroupID == "" || target.Peer == "" {
			return errors.New("target endpoint_id, group_id, and peer are required")
		}
		switch target.IncomingPeerType {
		case "user", "chat", "channel":
		default:
			return errors.New("target incoming_peer_type must be user, chat, or channel")
		}
		if target.IncomingPeerID <= 0 {
			return errors.New("target incoming_peer_id must be positive")
		}
		if target.TopicRootMessageID < 0 {
			return errors.New("target topic_root_message_id cannot be negative")
		}
		if _, ok := owned[target.EndpointID]; !ok {
			return fmt.Errorf("target endpoint %q is not owned by connector", target.EndpointID)
		}
		if _, ok := outbound[target.EndpointID]; ok {
			return fmt.Errorf("duplicate outbound target for endpoint %q", target.EndpointID)
		}
		outbound[target.EndpointID] = struct{}{}
		topics := inbound[target.PeerKey()]
		if topics == nil {
			topics = make(map[int]string)
			inbound[target.PeerKey()] = topics
		}
		if existing, ok := topics[target.TopicRootMessageID]; ok {
			return fmt.Errorf(
				"ambiguous inbound target for peer %s:%d topic %d: endpoints %q and %q",
				target.IncomingPeerType,
				target.IncomingPeerID,
				target.TopicRootMessageID,
				existing,
				target.EndpointID,
			)
		}
		if target.TopicRootMessageID == 0 && len(topics) != 0 {
			return fmt.Errorf("general inbound target for peer %s:%d conflicts with topic target", target.IncomingPeerType, target.IncomingPeerID)
		}
		if target.TopicRootMessageID != 0 {
			if _, ok := topics[0]; ok {
				return fmt.Errorf("topic inbound target for peer %s:%d conflicts with general target", target.IncomingPeerType, target.IncomingPeerID)
			}
		}
		topics[target.TopicRootMessageID] = target.EndpointID
	}
	return nil
}

func LoadCredentials() (Credentials, error) {
	appID, err := strconv.Atoi(os.Getenv("WUPHF_TELEGRAM_APP_ID"))
	if err != nil || appID <= 0 {
		return Credentials{}, errors.New("WUPHF_TELEGRAM_APP_ID must be a positive integer")
	}
	credentials := Credentials{
		AppID:    appID,
		AppHash:  os.Getenv("WUPHF_TELEGRAM_APP_HASH"),
		Phone:    os.Getenv("WUPHF_TELEGRAM_PHONE"),
		Password: os.Getenv("WUPHF_TELEGRAM_PASSWORD"),
		BotToken: os.Getenv("WUPHF_TELEGRAM_BOT_TOKEN"),
	}
	if credentials.AppHash == "" {
		return Credentials{}, errors.New("WUPHF_TELEGRAM_APP_HASH is required")
	}
	if credentials.BotToken == "" && credentials.Phone == "" {
		return Credentials{}, errors.New("set WUPHF_TELEGRAM_BOT_TOKEN or WUPHF_TELEGRAM_PHONE")
	}
	return credentials, nil
}
