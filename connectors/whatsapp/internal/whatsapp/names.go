package whatsappconnector

import (
	"context"
	"strings"
	"sync"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// titleTTL bounds how long a resolved chat title is reused before it is looked
// up again, so a group rename or a contact saved after the thread was created is
// picked up on the next refresh instead of being cached for the session.
const titleTTL = 5 * time.Minute

type cachedTitle struct {
	value string
	at    time.Time
}

// chatNamer resolves chat titles through the WhatsApp client and caches them
// with a short TTL, as the normalizer queries a title for every inbound message
// but the value only changes on a group rename, newsletter rename, or contact
// update.
type chatNamer struct {
	client *whatsmeow.Client
	mu     sync.RWMutex
	titles map[string]cachedTitle
}

func newChatNamer(client *whatsmeow.Client) *chatNamer {
	return &chatNamer{client: client, titles: make(map[string]cachedTitle)}
}

func (n *chatNamer) ChatTitle(ctx context.Context, chat types.JID) string {
	key := chat.String()
	n.mu.RLock()
	cached, ok := n.titles[key]
	n.mu.RUnlock()
	if ok && time.Since(cached.at) < titleTTL {
		return cached.value
	}
	title := n.resolve(ctx, chat)
	if title != "" {
		n.mu.Lock()
		n.titles[key] = cachedTitle{value: title, at: time.Now()}
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
		return tagged("G", n.groupName(ctx, chat))
	case types.NewsletterServer:
		return tagged("C", n.newsletterName(ctx, chat))
	default:
		return tagged("P", n.contactName(ctx, chat))
	}
}

// tagged prefixes a chat title with its kind ([G] group, [C] channel, [P]
// private) so a mirror topic shows what kind of chat it bridges. An empty title
// is left empty, as a bare tag would name nothing.
func tagged(kind, title string) string {
	if title == "" {
		return ""
	}
	return "[" + kind + "] " + title
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
// push name carried on the triggering message, preferred over the stored one. A
// LID-addressed person is mapped to its phone JID first, where the address-book
// contact and the real number live; without that step a saved contact would be
// shown as its raw LID id.
func (n *chatNamer) PersonName(ctx context.Context, jid types.JID, pushHint string) string {
	number := jid
	if jid.Server == types.HiddenUserServer {
		if pn, err := n.client.Store.LIDs.GetPNForLID(ctx, jid); err == nil && !pn.IsEmpty() {
			number = pn
		}
	}
	contact, err := n.client.Store.Contacts.GetContact(ctx, number)
	if err != nil {
		contact = types.ContactInfo{}
	}
	return formatPersonName(number, contact, pushHint)
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
