package whatsappconnector

import (
	"fmt"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	"go.mau.fi/whatsmeow/types"
)

type Target struct {
	EndpointID string
	GroupID    string
	Chat       types.JID
}

type TargetRegistry struct {
	outbound map[string]Target
	inbound  map[types.JID]Target
}

func NewTargetRegistry(cfg config.File) (TargetRegistry, error) {
	if err := cfg.Validate(); err != nil {
		return TargetRegistry{}, err
	}
	outbound := make(map[string]Target, len(cfg.Targets))
	inbound := make(map[types.JID]Target, len(cfg.Targets))
	for _, configured := range cfg.Targets {
		chat, err := types.ParseJID(configured.ChatJID)
		if err != nil {
			return TargetRegistry{}, fmt.Errorf("parse WhatsApp chat JID %q: %w", configured.ChatJID, err)
		}
		target := Target{EndpointID: configured.EndpointID, GroupID: configured.GroupID, Chat: chat}
		if existing, exists := inbound[chat]; exists {
			return TargetRegistry{}, fmt.Errorf("ambiguous normalized WhatsApp chat %s for endpoints %q and %q", chat, existing.EndpointID, target.EndpointID)
		}
		outbound[target.EndpointID] = target
		inbound[chat] = target
	}
	return TargetRegistry{outbound: outbound, inbound: inbound}, nil
}

func (r TargetRegistry) Outbound(endpointID string) (Target, bool) {
	target, ok := r.outbound[endpointID]
	return target, ok
}

func (r TargetRegistry) Incoming(chat types.JID) (Target, bool) {
	target, ok := r.inbound[chat]
	return target, ok
}
