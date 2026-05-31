package whatsappconnector

import (
	"context"
	"strings"
	"sync"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// chatNamer resolves chat titles through the WhatsApp client and caches them, as
// the normalizer queries a title for every inbound message but the value only
// changes on a group rename or contact update.
type chatNamer struct {
	client *whatsmeow.Client
	mu     sync.RWMutex
	titles map[string]string
}

func newChatNamer(client *whatsmeow.Client) *chatNamer {
	return &chatNamer{client: client, titles: make(map[string]string)}
}

func (n *chatNamer) ChatTitle(ctx context.Context, chat types.JID, isGroup bool) string {
	key := chat.String()
	n.mu.RLock()
	cached, ok := n.titles[key]
	n.mu.RUnlock()
	if ok {
		return cached
	}
	title := n.resolve(ctx, chat, isGroup)
	if title != "" {
		n.mu.Lock()
		n.titles[key] = title
		n.mu.Unlock()
	}
	return title
}

func (n *chatNamer) resolve(ctx context.Context, chat types.JID, isGroup bool) string {
	if isGroup {
		info, err := n.client.GetGroupInfo(ctx, chat)
		if err != nil || info == nil {
			return ""
		}
		return info.Name
	}
	contact, err := n.client.Store.Contacts.GetContact(ctx, chat)
	if err != nil || !contact.Found {
		return ""
	}
	for _, name := range []string{contact.FullName, contact.PushName, contact.BusinessName} {
		if trimmed := strings.TrimSpace(name); trimmed != "" {
			return trimmed
		}
	}
	return ""
}
