package whatsappconnector

import (
	"context"
	"sync"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// Addressing picks the address form (phone JID or LID) an outbound frame must use
// in a chat. The normalizer reports every address in its canonical form, so the
// core and its dispatches only ever hold that form; WhatsApp, however, keys a
// chat's messages by the form the chat is addressed in. A quote or message key
// naming a person in the other form does not link to the message it means: the
// reply arrives unlinked from its parent.
//
// So the canonical form is for identity, and this is its outbound counterpart:
// it records the form each chat arrives in and translates any address back to
// it before it goes on the wire.
type Addressing interface {
	// Mode is the form chat is addressed in.
	Mode(ctx context.Context, chat types.JID) types.AddressingMode
	// InMode returns jid in the given form when its counterpart is known, and
	// jid unchanged otherwise. Groups and other non-user addresses have one form
	// and are always returned unchanged.
	InMode(ctx context.Context, jid types.JID, mode types.AddressingMode) types.JID
	// Own is the paired account's address in the given form.
	Own(mode types.AddressingMode) types.JID
}

// addressRecorder learns a chat's form from the messages that arrive in it.
type addressRecorder interface {
	Observe(chat types.JID, source types.MessageSource)
}

// accountIdentity is the slice of the whatsmeow client the address book needs.
type accountIdentity interface {
	// OwnPN and OwnLID are the paired account's two addresses; either may be
	// empty before pairing completes.
	OwnPN() types.JID
	OwnLID() types.JID
	// LIDMigrated reports whether the account has moved its direct chats to
	// LIDs, in which case whatsmeow sends every direct message to the LID.
	LIDMigrated() bool
	// GroupMode asks WhatsApp how a group is addressed.
	GroupMode(ctx context.Context, group types.JID) (types.AddressingMode, error)
}

// addressBook is the Addressing used in production. A chat's form is taken, in
// order, from the last message that arrived in it, then from WhatsApp itself
// (group metadata, or for direct chats whether the account is LID-migrated), so
// a chat that has not spoken since a restart is still addressed correctly.
type addressBook struct {
	identity identityResolver
	account  accountIdentity
	mu       sync.RWMutex
	modes    map[types.JID]types.AddressingMode
}

func newAddressBook(identity identityResolver, account accountIdentity) *addressBook {
	return &addressBook{identity: identity, account: account, modes: make(map[types.JID]types.AddressingMode)}
}

// Observe records the form a message's chat arrived in, keyed by the chat's
// canonical address (the one dispatches will name). A group states its form on
// every message; a direct chat's form is the server its raw address is on.
func (b *addressBook) Observe(chat types.JID, source types.MessageSource) {
	mode := types.AddressingModePN
	switch {
	case source.IsGroup:
		if source.AddressingMode == types.AddressingModeLID {
			mode = types.AddressingModeLID
		}
	case source.Chat.Server == types.HiddenUserServer:
		mode = types.AddressingModeLID
	case source.Chat.Server != types.DefaultUserServer:
		// Newsletters and broadcasts have a single address form.
		return
	}
	b.remember(chat.ToNonAD(), mode)
}

func (b *addressBook) Mode(ctx context.Context, chat types.JID) types.AddressingMode {
	chat = chat.ToNonAD()
	switch chat.Server {
	case types.DefaultUserServer, types.HiddenUserServer:
		// whatsmeow sends every direct message of a migrated account to the
		// peer's LID, whatever address it is given, so that is the chat's form.
		if b.account.LIDMigrated() {
			return types.AddressingModeLID
		}
		if mode, ok := b.recorded(chat); ok {
			return mode
		}
		if chat.Server == types.HiddenUserServer {
			return types.AddressingModeLID
		}
		return types.AddressingModePN
	case types.GroupServer:
		if mode, ok := b.recorded(chat); ok {
			return mode
		}
		mode, err := b.account.GroupMode(ctx, chat)
		if err != nil {
			// Unknown: keep addresses as they are rather than guess.
			return ""
		}
		if mode != types.AddressingModeLID {
			mode = types.AddressingModePN
		}
		b.remember(chat, mode)
		return mode
	default:
		return ""
	}
}

func (b *addressBook) InMode(ctx context.Context, jid types.JID, mode types.AddressingMode) types.JID {
	jid = jid.ToNonAD()
	switch {
	case mode == types.AddressingModeLID && jid.Server == types.DefaultUserServer,
		mode == types.AddressingModePN && jid.Server == types.HiddenUserServer:
		if other := b.identity.Alternate(ctx, jid); !other.IsEmpty() {
			return other
		}
	}
	return jid
}

func (b *addressBook) Own(mode types.AddressingMode) types.JID {
	if mode == types.AddressingModeLID {
		if lid := b.account.OwnLID(); !lid.IsEmpty() {
			return lid.ToNonAD()
		}
	}
	return b.account.OwnPN().ToNonAD()
}

func (b *addressBook) recorded(chat types.JID) (types.AddressingMode, bool) {
	b.mu.RLock()
	defer b.mu.RUnlock()
	mode, ok := b.modes[chat]
	return mode, ok
}

func (b *addressBook) remember(chat types.JID, mode types.AddressingMode) {
	b.mu.Lock()
	b.modes[chat] = mode
	b.mu.Unlock()
}

// whatsmeowAccount reads the paired account's addresses and group metadata from
// a live whatsmeow client.
type whatsmeowAccount struct {
	client *whatsmeow.Client
}

func (a whatsmeowAccount) OwnPN() types.JID  { return a.client.Store.GetJID() }
func (a whatsmeowAccount) OwnLID() types.JID { return a.client.Store.GetLID() }

func (a whatsmeowAccount) LIDMigrated() bool {
	return a.client.Store != nil && a.client.Store.LIDMigrationTimestamp > 0
}

func (a whatsmeowAccount) GroupMode(ctx context.Context, group types.JID) (types.AddressingMode, error) {
	info, err := a.client.GetGroupInfo(ctx, group)
	if err != nil {
		return "", err
	}
	return info.AddressingMode, nil
}
