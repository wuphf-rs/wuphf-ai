package telegramconnector

import (
	"fmt"

	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	"github.com/gotd/td/tg"
)

type TargetRegistry struct {
	byEndpoint map[string]config.Target
	byPeer     map[config.PeerKey][]config.Target
}

func NewTargetRegistry(cfg config.File) (TargetRegistry, error) {
	if err := cfg.Validate(); err != nil {
		return TargetRegistry{}, err
	}
	registry := TargetRegistry{
		byEndpoint: make(map[string]config.Target, len(cfg.Targets)),
		byPeer:     make(map[config.PeerKey][]config.Target, len(cfg.Targets)),
	}
	for _, target := range cfg.Targets {
		registry.byEndpoint[target.EndpointID] = target
		registry.byPeer[target.PeerKey()] = append(registry.byPeer[target.PeerKey()], target)
	}
	return registry, nil
}

func (r TargetRegistry) Outbound(endpointID string) (config.Target, bool) {
	target, ok := r.byEndpoint[endpointID]
	return target, ok
}

func (r TargetRegistry) Incoming(peer tg.PeerClass) []config.Target {
	return r.byPeer[peerKey(peer)]
}

func peerKey(peer tg.PeerClass) config.PeerKey {
	switch value := peer.(type) {
	case *tg.PeerUser:
		return config.PeerKey{Type: "user", ID: value.UserID}
	case *tg.PeerChat:
		return config.PeerKey{Type: "chat", ID: value.ChatID}
	case *tg.PeerChannel:
		return config.PeerKey{Type: "channel", ID: value.ChannelID}
	default:
		return config.PeerKey{}
	}
}

func formatPeer(peer tg.PeerClass) string {
	key := peerKey(peer)
	if key.Type == "" {
		return "telegram:unknown"
	}
	return fmt.Sprintf("telegram:%s:%d", key.Type, key.ID)
}
