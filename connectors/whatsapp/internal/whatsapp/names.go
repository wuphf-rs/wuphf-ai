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
// changes on a group rename, newsletter rename, or contact update.
type chatNamer struct {
	client *whatsmeow.Client
	mu     sync.RWMutex
	titles map[string]string
}

func newChatNamer(client *whatsmeow.Client) *chatNamer {
	return &chatNamer{client: client, titles: make(map[string]string)}
}

func (n *chatNamer) ChatTitle(ctx context.Context, chat types.JID) string {
	key := chat.String()
	n.mu.RLock()
	cached, ok := n.titles[key]
	n.mu.RUnlock()
	if ok {
		return cached
	}
	title := n.resolve(ctx, chat)
	if title != "" {
		n.mu.Lock()
		n.titles[key] = title
		n.mu.Unlock()
	}
	return title
}

// resolve picks a title strategy from the chat's address space. Each WhatsApp
// chat kind exposes its name through a different client call, so dispatching on
// the JID server keeps adding a new kind to a single switch arm.
func (n *chatNamer) resolve(ctx context.Context, chat types.JID) string {
	switch chat.Server {
	case types.GroupServer:
		return n.groupName(ctx, chat)
	case types.NewsletterServer:
		return n.newsletterName(ctx, chat)
	default:
		return n.contactName(ctx, chat)
	}
}

func (n *chatNamer) groupName(ctx context.Context, chat types.JID) string {
	info, err := n.client.GetGroupInfo(ctx, chat)
	if err != nil || info == nil {
		return ""
	}
	return info.Name
}

func (n *chatNamer) newsletterName(ctx context.Context, chat types.JID) string {
	info, err := n.client.GetNewsletterInfo(ctx, chat)
	if err != nil || info == nil {
		return ""
	}
	return info.ThreadMeta.Name.Text
}

func (n *chatNamer) contactName(ctx context.Context, chat types.JID) string {
	return n.PersonName(ctx, chat, "")
}

// PersonName resolves a person's display name for a mirror header (the sender of
// a group message) or a direct-chat thread title. The pushHint is the freshest
// push name carried on the triggering message, preferred over the stored one.
func (n *chatNamer) PersonName(ctx context.Context, jid types.JID, pushHint string) string {
	contact, err := n.client.Store.Contacts.GetContact(ctx, jid)
	if err != nil {
		return formatPersonName(jid, types.ContactInfo{}, pushHint)
	}
	return formatPersonName(jid, contact, pushHint)
}

// formatPersonName renders a person by descending specificity: a saved contact
// shows its address-book name alone; a business shows its name tagged
// "(Business)" instead of an id; an unsaved contact shows its push name with
// the phone number / id in brackets, or just the number when no push name is
// known.
func formatPersonName(jid types.JID, contact types.ContactInfo, pushHint string) string {
	if full := strings.TrimSpace(contact.FullName); full != "" {
		return full
	}
	if business := strings.TrimSpace(contact.BusinessName); business != "" {
		return business + " (Business)"
	}
	push := strings.TrimSpace(pushHint)
	if push == "" {
		push = strings.TrimSpace(contact.PushName)
	}
	if push != "" {
		return push + " (" + jid.User + ")"
	}
	return jid.User
}
