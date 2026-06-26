package whatsappconnector

import (
	"context"
	"strings"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// ChatNamer resolves a human-readable title for a chat so the core can label a
// freshly provisioned mirror thread. The chat kind (group, newsletter, contact)
// is derived from the JID, so callers need not classify it. Implementations are
// expected to cache, as it is queried per inbound message.
type ChatNamer interface {
	ChatTitle(ctx context.Context, chat types.JID) string
	// PersonName resolves a sender's display name for a mirror header. The
	// pushHint is the push name carried on the triggering message.
	PersonName(ctx context.Context, jid types.JID, pushHint string) string
}

type Normalizer struct {
	inputs   chan<- *wuphfpb.ClientFrame
	media    MediaCodec
	namer    ChatNamer
	identity identityResolver
	// migrations dedupes the phone-to-LID address changes already reported to
	// the core, so a long-lived chat does not re-emit the same migration on
	// every message. It is a pointer so the shared gate survives the value
	// receiver copies the normalizer is invoked through.
	migrations *migrationGate
}

func NewNormalizer(inputs chan<- *wuphfpb.ClientFrame, media MediaCodec, namer ChatNamer, identity identityResolver) Normalizer {
	return Normalizer{
		inputs:     inputs,
		media:      media,
		namer:      namer,
		identity:   identity,
		migrations: newMigrationGate(),
	}
}

// canonical resolves a chat/peer/sender JID to its stable bridge identity. The
// device is always stripped, even without a resolver, so a per-device AD-JID
// never splits a chat; LID promotion needs the resolver and is skipped when none
// is wired (as in tests).
func (n Normalizer) canonical(ctx context.Context, jid types.JID) types.JID {
	if n.identity == nil {
		return jid.ToNonAD()
	}
	return n.identity.Canonical(ctx, jid)
}

func (n Normalizer) Handle(ctx context.Context, message *events.Message) error {
	// The chat is canonicalized once and threaded through every frame below so
	// creates, edits, and deletes for one peer always carry the same chat id no
	// matter which address form WhatsApp used for this particular message.
	chat := n.canonical(ctx, message.Info.Chat)
	// Any phone/LID migration must be reported before the message so the core
	// can bind both addresses to one conversation before resolving this event.
	for _, migration := range n.migrationsFor(message, chat) {
		if err := n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_IdentityMigrated{IdentityMigrated: migration},
		}); err != nil {
			return err
		}
	}
	if reaction, ok := n.normalizeReaction(ctx, message, chat); ok {
		return n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_Reaction{Reaction: reaction},
		})
	}
	if del, ok := n.normalizeDelete(ctx, message); ok {
		return n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_MessageDelete{MessageDelete: del},
		})
	}
	if message.IsEdit {
		edit, ok := n.normalizeEdit(ctx, message)
		if !ok {
			return nil
		}
		return n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_MessageEdit{MessageEdit: edit},
		})
	}
	event, ok, err := n.normalize(ctx, message)
	if err != nil || !ok {
		return err
	}
	return n.send(ctx, &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageCreate{MessageCreate: event},
	})
}

// migrationsFor collects the chat identity migrations implied by this message,
// deduped against ones already reported. Two signals feed it: WhatsApp's own
// alt-address hint (RecipientAlt) and a canonical shift detected here, where the
// chat's raw address differs from its resolved identity because the LID mapping
// was learned after an earlier message already minted an endpoint under the
// phone JID. Reporting the shift rebinds that stale endpoint instead of letting
// the LID form mint a second mirror thread.
func (n Normalizer) migrationsFor(message *events.Message, chat types.JID) []*wuphfpb.IdentityMigrated {
	var migrations []*wuphfpb.IdentityMigrated
	if migration := identityMigration(message); migration != nil {
		migrations = append(migrations, migration)
	}
	if !message.Info.IsGroup {
		if raw := message.Info.Chat.String(); raw != chat.String() {
			migrations = append(migrations, &wuphfpb.IdentityMigrated{
				OldChatId: raw,
				NewChatId: chat.String(),
			})
		}
	}
	return n.migrations.unseen(migrations)
}

// normalizeEdit reports an inbound content revision. WhatsApp carries an edit as
// a protocol message whose key names the original message and whose edited body
// holds the new content; the core suppresses no-op and echoed edits by comparing
// text, so only the original message id and new text are needed here.
func (n Normalizer) normalizeEdit(ctx context.Context, message *events.Message) (*wuphfpb.MessageEditEvent, bool) {
	protocol := message.Message.GetProtocolMessage()
	if protocol == nil {
		return nil, false
	}
	originalID := protocol.GetKey().GetID()
	if originalID == "" {
		return nil, false
	}
	event := &wuphfpb.MessageEditEvent{
		ChatId:            n.canonical(ctx, message.Info.Chat).String(),
		PlatformMessageId: originalID,
	}
	if text := messageText(protocol.GetEditedMessage()); text != "" {
		event.Text = &text
	}
	return event, true
}

// normalizeDelete reports an inbound revoke. WhatsApp carries a delete as a
// protocol message of type REVOKE whose key names the revoked message; only the
// original message id is needed, as the core resolves it to the logical message
// and fans the deletion out to the other copies.
func (n Normalizer) normalizeDelete(ctx context.Context, message *events.Message) (*wuphfpb.MessageDeleteEvent, bool) {
	protocol := message.Message.GetProtocolMessage()
	if protocol == nil || protocol.GetType() != waE2E.ProtocolMessage_REVOKE {
		return nil, false
	}
	revokedID := protocol.GetKey().GetID()
	if revokedID == "" {
		return nil, false
	}
	return &wuphfpb.MessageDeleteEvent{
		ChatId:            n.canonical(ctx, message.Info.Chat).String(),
		PlatformMessageId: revokedID,
	}, true
}

// HandleReceipt reports a read receipt as a seen signal. Only peer reads in a
// direct chat are bridged: seen receipts are a DM feature, and group reads are
// per-participant noise. The core surfaces it on the origin copy (a 👀 reaction
// on the Telegram side); the chat and reader are canonicalized like any id.
func (n Normalizer) HandleReceipt(ctx context.Context, receipt *events.Receipt) error {
	if receipt.Type != types.ReceiptTypeRead || receipt.IsGroup {
		return nil
	}
	chat := n.canonical(ctx, receipt.Chat).String()
	reader := n.canonical(ctx, receipt.Sender).String()
	for _, id := range receipt.MessageIDs {
		event := &wuphfpb.ReadReceipt{
			ChatId:            chat,
			PlatformMessageId: string(id),
			Reader:            reader,
			FromMe:            receipt.IsFromMe,
		}
		if err := n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_ReadReceipt{ReadReceipt: event},
		}); err != nil {
			return err
		}
	}
	return nil
}

// normalizeReaction reports an inbound reaction. WhatsApp carries a reaction as
// a message whose ReactionMessage names the reacted message and holds the emoji
// (empty text clears the reactor's reaction). The reactor is the message sender,
// canonicalized like any other id; the core resolves the reacted message and
// fans the reaction to the other endpoints. Encrypted group reactions
// (EncReactionMessage) need the client to decrypt and are not handled here yet.
func (n Normalizer) normalizeReaction(ctx context.Context, message *events.Message, chat types.JID) (*wuphfpb.ReactionEvent, bool) {
	reaction := message.Message.GetReactionMessage()
	if reaction == nil {
		return nil, false
	}
	target := reaction.GetKey().GetID()
	if target == "" {
		return nil, false
	}
	sender := n.canonical(ctx, message.Info.Sender).String()
	event := &wuphfpb.ReactionEvent{
		ChatId:            chat.String(),
		PlatformMessageId: target,
		Sender:            sender,
		Emoji:             reaction.GetText(),
		FromMe:            message.Info.IsFromMe,
	}
	if name := n.senderName(ctx, message); name != "" {
		event.SenderName = &name
	}
	return event, true
}

// senderName resolves the per-message sender label, formatted by the namer when
// present and falling back to the raw push name otherwise. The account owner's
// own messages are labelled "Me" rather than their profile name, so a direct
// chat's two sides stay distinguishable at a glance on the mirror.
//
// Any sender whose device is not the primary phone (device 0) is tagged as
// writing from a linked device (WhatsApp Web/Desktop), the account owner and
// other people alike. Bridge-authored sends never reach this point, as the
// core drops them as sent-copy echoes before dispatching.
func (n Normalizer) senderName(ctx context.Context, message *events.Message) string {
	name := ""
	if message.Info.IsFromMe {
		name = "Me"
	} else if n.namer != nil {
		name = n.namer.PersonName(ctx, message.Info.Sender, message.Info.PushName)
	}
	if name == "" {
		name = strings.TrimSpace(message.Info.PushName)
	}
	if name != "" && message.Info.Sender.Device != 0 {
		name += " (linked device)"
	}
	return name
}

func (n Normalizer) send(ctx context.Context, frame *wuphfpb.ClientFrame) error {
	select {
	case n.inputs <- frame:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (n Normalizer) normalize(ctx context.Context, message *events.Message) (*wuphfpb.MessageCreateEvent, bool, error) {
	if message.IsEdit {
		return nil, false, nil
	}
	attachments, supported, err := n.media.Inbound(ctx, message.Message)
	if err != nil {
		return nil, false, err
	}
	if hasAttachment(message.Message) && !supported {
		return nil, false, nil
	}
	text := messageText(message.Message)
	if text == "" && len(attachments) == 0 {
		return nil, false, nil
	}
	// The sender is canonicalized like the chat so one author is not shown as
	// two identities when WhatsApp reports them under a phone JID on one message
	// and a LID on the next.
	sender := n.canonical(ctx, message.Info.Sender).String()
	event := &wuphfpb.MessageCreateEvent{
		ChatId:            n.canonical(ctx, message.Info.Chat).String(),
		PlatformMessageId: string(message.Info.ID),
		Sender:            sender,
		Attachments:       attachments,
		TimestampMs:       message.Info.Timestamp.UnixMilli(),
	}
	if n.namer != nil {
		if title := n.namer.ChatTitle(ctx, message.Info.Chat); title != "" {
			event.ChatTitle = &title
		}
	}
	// The participant is set for every chat kind, not only groups: mirror
	// targets label a message with its author only when a participant is
	// present, and a direct chat shows both sides of the conversation, so
	// without the label the account owner's messages and the peer's are
	// indistinguishable on the mirror.
	participant := sender
	event.Participant = &participant
	if name := n.senderName(ctx, message); name != "" {
		event.SenderName = &name
	}
	if text != "" {
		event.Text = &text
	}
	if context := messageContext(message.Message); context != nil && context.GetStanzaID() != "" {
		reply := context.GetStanzaID()
		event.ReplyToPlatformMessageId = &reply
	}
	return event, true, nil
}

// identityMigration derives a phone-to-LID (or reverse) chat address change for
// a direct message. WhatsApp keeps both addresses live, so the core must learn
// they refer to one conversation. The LID form is treated as canonical.
func identityMigration(message *events.Message) *wuphfpb.IdentityMigrated {
	if message.Info.IsGroup || message.Info.RecipientAlt.IsEmpty() {
		return nil
	}
	var oldJID, newJID types.JID
	if message.Info.AddressingMode == types.AddressingModeLID {
		newJID, oldJID = message.Info.Chat, message.Info.RecipientAlt
	} else {
		oldJID, newJID = message.Info.Chat, message.Info.RecipientAlt
	}
	if oldJID.IsEmpty() || newJID.IsEmpty() || oldJID == newJID {
		return nil
	}
	return &wuphfpb.IdentityMigrated{
		OldChatId: oldJID.String(),
		NewChatId: newJID.String(),
	}
}

func messageText(message *waE2E.Message) string {
	switch {
	case message.GetConversation() != "":
		return message.GetConversation()
	case message.GetExtendedTextMessage() != nil:
		return message.GetExtendedTextMessage().GetText()
	case message.GetImageMessage() != nil:
		return message.GetImageMessage().GetCaption()
	case message.GetDocumentMessage() != nil:
		return message.GetDocumentMessage().GetCaption()
	case message.GetVideoMessage() != nil:
		return message.GetVideoMessage().GetCaption()
	default:
		return ""
	}
}

func messageContext(message *waE2E.Message) *waE2E.ContextInfo {
	switch {
	case message.GetExtendedTextMessage() != nil:
		return message.GetExtendedTextMessage().GetContextInfo()
	case message.GetImageMessage() != nil:
		return message.GetImageMessage().GetContextInfo()
	case message.GetDocumentMessage() != nil:
		return message.GetDocumentMessage().GetContextInfo()
	case message.GetVideoMessage() != nil:
		return message.GetVideoMessage().GetContextInfo()
	case message.GetAudioMessage() != nil:
		return message.GetAudioMessage().GetContextInfo()
	case message.GetStickerMessage() != nil:
		return message.GetStickerMessage().GetContextInfo()
	default:
		return nil
	}
}

func hasAttachment(message *waE2E.Message) bool {
	return message.GetImageMessage() != nil ||
		message.GetDocumentMessage() != nil ||
		message.GetVideoMessage() != nil ||
		message.GetAudioMessage() != nil ||
		message.GetStickerMessage() != nil
}
