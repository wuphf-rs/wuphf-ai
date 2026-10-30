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

// messageDecryptor opens what WhatsApp seals with the target message's secret,
// which only the client holding the session can open:
//   - group reactions, which travel as an EncReactionMessage;
//   - SecretEncryptedMessage, which carries, among others, edits made on the
//     account's main device.
//
// Kept as a seam so the normalizer stays testable without a live client; a nil
// decryptor leaves such messages unreported.
type messageDecryptor interface {
	DecryptReaction(ctx context.Context, message *events.Message) (*waE2E.ReactionMessage, error)
	DecryptSecretEncryptedMessage(ctx context.Context, message *events.Message) (*waE2E.Message, error)
}

type Normalizer struct {
	inputs    chan<- *wuphfpb.ClientFrame
	media     MediaCodec
	namer     ChatNamer
	identity  identityResolver
	decryptor messageDecryptor
	// migrations dedupes the phone-to-LID address changes already reported to
	// the core, so a long-lived chat does not re-emit the same migration on
	// every message. It is a pointer so the shared gate survives the value
	// receiver copies the normalizer is invoked through.
	migrations *migrationGate
	// addresses learns the form (phone JID or LID) each chat arrives in, so
	// the sender can address the chat the same way. Nil in tests that do not
	// exercise it.
	addresses addressRecorder
}

func NewNormalizer(
	inputs chan<- *wuphfpb.ClientFrame,
	media MediaCodec,
	namer ChatNamer,
	identity identityResolver,
	decryptor messageDecryptor,
	addresses addressRecorder,
) Normalizer {
	return Normalizer{
		inputs:     inputs,
		media:      media,
		namer:      namer,
		identity:   identity,
		decryptor:  decryptor,
		addresses:  addresses,
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
	if n.addresses != nil {
		n.addresses.Observe(chat, message.Info.MessageSource)
	}
	// View-once photos and videos arrive wrapped in a container; unwrap to the
	// inner media so they bridge as the ordinary attachment they hold.
	message.Message = unwrapViewOnce(message.Message)
	// Any phone/LID migration must be reported before the message so the core
	// can bind both addresses to one conversation before resolving this event.
	for _, migration := range n.migrationsFor(ctx, message, chat) {
		if err := n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_IdentityMigrated{IdentityMigrated: migration},
		}); err != nil {
			return err
		}
	}
	// A SecretEncryptedMessage carries no content of its own: it is a revision
	// sealed with the target message's secret, e.g. an edit made on the
	// account's main device. Only edits are bridged; poll and event revisions
	// are dropped like the polls and events they revise.
	if message.Message.GetSecretEncryptedMessage() != nil {
		edit, ok := n.normalizeSecretEdit(ctx, message, chat)
		if !ok {
			return nil
		}
		return n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_MessageEdit{MessageEdit: edit},
		})
	}
	// A timer change is a protocol message about the chat, not a message to
	// bridge: it updates the chat's profile in the core.
	if timer, ok := ephemeralSetting(message); ok {
		return n.sendChatUpdates(ctx, []*wuphfpb.ChatUpdate{{ChatId: chat.String(), Disappearing: timer}})
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
// deduped against ones already reported. Three signals feed it: WhatsApp's own
// alt-address hint (RecipientAlt); a canonical shift detected here, where the
// chat's raw address differs from its resolved identity because the LID mapping
// was learned after an earlier message already minted an endpoint under the
// phone JID; and the counterpart the LID store holds for the canonical address.
// Reporting the shift rebinds a stale endpoint instead of letting the LID form
// mint a second mirror thread; reporting the counterpart teaches the core both
// forms of the chat even when nothing migrated, so a routing rule that names it
// by phone number still matches a LID-addressed message.
func (n Normalizer) migrationsFor(
	ctx context.Context,
	message *events.Message,
	chat types.JID,
) []*wuphfpb.IdentityMigrated {
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
		if alternate := n.alternate(ctx, chat); !alternate.IsEmpty() && alternate != chat {
			migrations = append(migrations, &wuphfpb.IdentityMigrated{
				OldChatId: alternate.String(),
				NewChatId: chat.String(),
			})
		}
	}
	return n.migrations.unseen(migrations)
}

// alternate returns the chat's other address form, or an empty JID when the
// resolver is absent (as in tests) or knows no counterpart.
func (n Normalizer) alternate(ctx context.Context, jid types.JID) types.JID {
	if n.identity == nil {
		return types.EmptyJID
	}
	return n.identity.Alternate(ctx, jid)
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

// normalizeSecretEdit opens a sealed edit and reports it like a plain one: the
// target key names the original message, and the opened message holds the new
// content, either directly or wrapped as a protocol or edited message.
func (n Normalizer) normalizeSecretEdit(ctx context.Context, message *events.Message, chat types.JID) (*wuphfpb.MessageEditEvent, bool) {
	secret := message.Message.GetSecretEncryptedMessage()
	if n.decryptor == nil || secret.GetSecretEncType() != waE2E.SecretEncryptedMessage_MESSAGE_EDIT {
		return nil, false
	}
	originalID := secret.GetTargetMessageKey().GetID()
	if originalID == "" {
		return nil, false
	}
	opened, err := n.decryptor.DecryptSecretEncryptedMessage(ctx, message)
	if err != nil {
		return nil, false
	}
	content := opened
	if edited := opened.GetProtocolMessage().GetEditedMessage(); edited != nil {
		content = edited
	} else if edited := opened.GetEditedMessage().GetMessage(); edited != nil {
		content = edited
	}
	event := &wuphfpb.MessageEditEvent{
		ChatId:            chat.String(),
		PlatformMessageId: originalID,
	}
	if text := messageText(content); text != "" {
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

// HandleReceipt reports a delivery or read receipt. Only direct-chat receipts
// are bridged: these are a DM feature, and group receipts are per-participant
// noise. A delivered receipt surfaces as a 👍 and a read receipt as a 👀 on the
// message's mirror copy. This covers messages the bridge sent and messages the
// account owner sent from another device alike, as the peer's receipt names the
// message either way; the core ignores the owner's own (from_me) receipts. The
// chat and observer are canonicalized like any id.
func (n Normalizer) HandleReceipt(ctx context.Context, receipt *events.Receipt) error {
	if receipt.IsGroup {
		return nil
	}
	kind, ok := receiptKind(receipt.Type)
	if !ok {
		return nil
	}
	chat := n.canonical(ctx, receipt.Chat).String()
	observer := n.canonical(ctx, receipt.Sender).String()
	for _, id := range receipt.MessageIDs {
		event := &wuphfpb.ReceiptEvent{
			ChatId:            chat,
			PlatformMessageId: string(id),
			Observer:          observer,
			FromMe:            receipt.IsFromMe,
			Kind:              kind,
		}
		if err := n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_Receipt{Receipt: event},
		}); err != nil {
			return err
		}
	}
	return nil
}

// receiptKind maps a whatsmeow receipt type to the bridge's receipt kind,
// reporting whether it is one the bridge surfaces. Delivered (the default,
// empty type) and read are surfaced; sender/retry/played and self-reads carry
// no peer-acknowledgement meaning for the mirror and are dropped.
func receiptKind(t types.ReceiptType) (wuphfpb.ReceiptKind, bool) {
	switch t {
	case types.ReceiptTypeDelivered:
		return wuphfpb.ReceiptKind_RECEIPT_KIND_DELIVERED, true
	case types.ReceiptTypeRead:
		return wuphfpb.ReceiptKind_RECEIPT_KIND_READ, true
	default:
		return wuphfpb.ReceiptKind_RECEIPT_KIND_UNSPECIFIED, false
	}
}

// normalizeReaction reports an inbound reaction. WhatsApp carries a reaction as
// a message whose ReactionMessage names the reacted message and holds the emoji
// (empty text clears the reactor's reaction). The reactor is the message sender,
// canonicalized like any other id; the core resolves the reacted message and
// fans the reaction to the other endpoints.
func (n Normalizer) normalizeReaction(ctx context.Context, message *events.Message, chat types.JID) (*wuphfpb.ReactionEvent, bool) {
	reaction, ok := n.reactionMessage(ctx, message)
	if !ok {
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

// reactionMessage extracts the reaction a message carries, decrypting it when
// needed. Reactions in groups with message-secret encryption arrive as an
// EncReactionMessage whose payload only the client can open, so those are
// decrypted through the decryptor; a plaintext ReactionMessage is used as-is.
// A decryption failure is reported as "no reaction" rather than an error: the
// message is then handled as an ordinary (empty) message and dropped, which is
// what happened to every encrypted reaction before they were decrypted at all.
func (n Normalizer) reactionMessage(ctx context.Context, message *events.Message) (*waE2E.ReactionMessage, bool) {
	if reaction := message.Message.GetReactionMessage(); reaction != nil {
		return reaction, true
	}
	if message.Message.GetEncReactionMessage() == nil || n.decryptor == nil {
		return nil, false
	}
	reaction, err := n.decryptor.DecryptReaction(ctx, message)
	if err != nil || reaction == nil {
		return nil, false
	}
	return reaction, true
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
	text := markViewOnce(message.Message, messageText(message.Message))
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
	event.Disappearing = messageTimer(message.Message)
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

// unwrapViewOnce returns the inner message of a view-once container so a
// view-once photo or video is bridged as the ordinary media it wraps. WhatsApp
// delivers view-once content inside one of three container variants, each
// holding a normal ImageMessage/VideoMessage (flagged ViewOnce) that downloads
// like any other media. A message that is not a view-once container is returned
// unchanged.
func unwrapViewOnce(message *waE2E.Message) *waE2E.Message {
	switch {
	case message == nil:
		return message
	case message.GetViewOnceMessage().GetMessage() != nil:
		return message.GetViewOnceMessage().GetMessage()
	case message.GetViewOnceMessageV2().GetMessage() != nil:
		return message.GetViewOnceMessageV2().GetMessage()
	case message.GetViewOnceMessageV2Extension().GetMessage() != nil:
		return message.GetViewOnceMessageV2Extension().GetMessage()
	default:
		return message
	}
}

// viewOnceMarker labels a bridged view-once photo, video, or voice note so the
// mirror copy is recognisable as one (the original self-destructs on WhatsApp).
const viewOnceMarker = "👁️ View once"

// markViewOnce prefixes the marker to a message's caption when the message is a
// view-once one, so the mirror copy is flagged. A captionless view-once becomes
// just the marker; a non-view-once caption is returned unchanged.
func markViewOnce(message *waE2E.Message, text string) string {
	if !isViewOnce(message) {
		return text
	}
	if text == "" {
		return viewOnceMarker
	}
	return viewOnceMarker + "\n" + text
}

// isViewOnce reports whether a message is a view-once photo, video, or voice
// note. WhatsApp flags the inner media (which unwrapViewOnce has already exposed)
// with ViewOnce, so the flag is read straight off the media regardless of which
// container delivered it.
func isViewOnce(message *waE2E.Message) bool {
	return message.GetImageMessage().GetViewOnce() ||
		message.GetVideoMessage().GetViewOnce() ||
		message.GetAudioMessage().GetViewOnce()
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

// chatUpdateBatch is the most chat updates one frame carries. A history sync
// reports every conversation at once; batching keeps each frame well under
// gRPC limits.
const chatUpdateBatch = 500

// messageTimer is the chat's disappearing timer as an inbound message carried
// it: ContextInfo.Expiration when greater than 0, with its setting timestamp
// when present. A message without one reports nil, which never turns a timer
// off. The owner's own messages carry the timer too.
func messageTimer(message *waE2E.Message) *wuphfpb.DisappearingTimer {
	context := messageContext(message)
	if context.GetExpiration() == 0 {
		return nil
	}
	timer := &wuphfpb.DisappearingTimer{Seconds: context.GetExpiration()}
	if at := context.GetEphemeralSettingTimestamp(); at > 0 {
		timer.SettingTimestamp = &at
	}
	return timer
}

// ephemeralSetting reads an EPHEMERAL_SETTING protocol message: the chat's new
// timer (0 = off), set at the message's setting timestamp or, when it has
// none, at the message's own time.
func ephemeralSetting(message *events.Message) (*wuphfpb.DisappearingTimer, bool) {
	protocol := message.Message.GetProtocolMessage()
	if protocol.GetType() != waE2E.ProtocolMessage_EPHEMERAL_SETTING {
		return nil, false
	}
	at := protocol.GetEphemeralSettingTimestamp()
	if at <= 0 {
		at = message.Info.Timestamp.Unix()
	}
	return &wuphfpb.DisappearingTimer{
		Seconds:          protocol.GetEphemeralExpiration(),
		SettingTimestamp: &at,
	}, true
}

// sendChatUpdates queues chat updates for the core in frames of at most
// chatUpdateBatch updates.
func (n Normalizer) sendChatUpdates(ctx context.Context, updates []*wuphfpb.ChatUpdate) error {
	for start := 0; start < len(updates); start += chatUpdateBatch {
		end := min(start+chatUpdateBatch, len(updates))
		if err := n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_ChatUpdate{ChatUpdate: &wuphfpb.ChatUpdateEvent{
				Updates: updates[start:end],
			}},
		}); err != nil {
			return err
		}
	}
	return nil
}

// HandleGroupInfo reports a group's disappearing timer when a group
// notification changes it. Other group changes report nothing.
func (n Normalizer) HandleGroupInfo(ctx context.Context, evt *events.GroupInfo) error {
	if evt.Ephemeral == nil {
		return nil
	}
	timer := &wuphfpb.DisappearingTimer{}
	if evt.Ephemeral.IsEphemeral {
		timer.Seconds = evt.Ephemeral.DisappearingTimer
	}
	if !evt.Timestamp.IsZero() {
		at := evt.Timestamp.Unix()
		timer.SettingTimestamp = &at
	}
	return n.sendChatUpdates(ctx, []*wuphfpb.ChatUpdate{{
		ChatId:       n.canonical(ctx, evt.JID).String(),
		Disappearing: timer,
	}})
}

// HandleHistorySync reports every DM's and group's timer in a history sync. A
// sync snapshot omits the expiration when the timer is off, so an absent one
// reports 0. Chat ids are canonicalized exactly like a message's chat, so the
// core keys the profile under the id it routes.
func (n Normalizer) HandleHistorySync(ctx context.Context, evt *events.HistorySync) error {
	var updates []*wuphfpb.ChatUpdate
	for _, conversation := range evt.Data.GetConversations() {
		jid, err := types.ParseJID(conversation.GetID())
		if err != nil {
			continue
		}
		switch jid.Server {
		case types.DefaultUserServer, types.HiddenUserServer, types.GroupServer:
		default:
			continue
		}
		timer := &wuphfpb.DisappearingTimer{Seconds: conversation.GetEphemeralExpiration()}
		if at := conversation.GetEphemeralSettingTimestamp(); at > 0 {
			timer.SettingTimestamp = &at
		}
		updates = append(updates, &wuphfpb.ChatUpdate{
			ChatId:       n.canonical(ctx, jid).String(),
			Disappearing: timer,
		})
	}
	return n.sendChatUpdates(ctx, updates)
}
