package telegramconnector

import (
	"context"
	"fmt"
	"strconv"
	"sync"
	"time"

	"github.com/gotd/td/tg"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

type Normalizer struct {
	peers  *PeerCache
	inputs chan<- *wuphfpb.ClientFrame
	media  MediaCodec
	// panel, when set, takes the messages that drive the settings panel
	// before anything is mirrored; taken remembers them so that their edits
	// are not mirrored either.
	panel PanelRouter
	taken *takenMessages
	// reactions, when set, reports other accounts' reactions (plan 080).
	reactions *Reactions
}

// PanelRouter takes the messages that drive the settings panel: the panel
// command and answers to a pending question. It reports whether it took the
// message; a taken message is never mirrored.
type PanelRouter interface {
	Take(ctx context.Context, entities tg.Entities, message *tg.Message) bool
}

func NewNormalizer(
	peers *PeerCache,
	inputs chan<- *wuphfpb.ClientFrame,
	media MediaCodec,
) Normalizer {
	return Normalizer{peers: peers, inputs: inputs, media: media}
}

// WithReactions returns the normalizer reporting reactions through reactions.
func (n Normalizer) WithReactions(reactions *Reactions) Normalizer {
	n.reactions = reactions
	return n
}

// WithPanel returns the normalizer with router taking the settings panel's
// messages before they are mirrored.
func (n Normalizer) WithPanel(router PanelRouter) Normalizer {
	n.panel = router
	n.taken = newTakenMessages(takenMessageTTL)
	return n
}

func (n Normalizer) Dispatcher() tg.UpdateDispatcher {
	dispatcher := tg.NewUpdateDispatcher()
	dispatcher.OnNewMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateNewMessage) error {
		return n.handle(ctx, entities, update.Message)
	})
	dispatcher.OnNewChannelMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateNewChannelMessage) error {
		// A topic closed or reopened is a service message about the topic,
		// never a message to mirror (handle drops service messages).
		if err := n.sendChatUpdates(ctx, topicCloseUpdates(update.Message)); err != nil {
			return err
		}
		return n.handle(ctx, entities, update.Message)
	})
	// Topic pin and mute state (mirror topics only; see chatstate.go).
	dispatcher.OnPinnedForumTopic(func(ctx context.Context, _ tg.Entities, update *tg.UpdatePinnedForumTopic) error {
		return n.sendChatUpdates(ctx, pinnedTopicUpdates(update))
	})
	dispatcher.OnPinnedForumTopics(func(ctx context.Context, _ tg.Entities, update *tg.UpdatePinnedForumTopics) error {
		return n.sendChatUpdates(ctx, pinnedTopicsUpdates(update))
	})
	dispatcher.OnNotifySettings(func(ctx context.Context, _ tg.Entities, update *tg.UpdateNotifySettings) error {
		return n.sendChatUpdates(ctx, notifyUpdates(update, n.peers.IsBot(), time.Now()))
	})
	dispatcher.OnEditMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateEditMessage) error {
		return n.handleEdit(ctx, entities, update.Message)
	})
	dispatcher.OnEditChannelMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateEditChannelMessage) error {
		return n.handleEdit(ctx, entities, update.Message)
	})
	if n.reactions != nil {
		dispatcher.OnMessageReactions(func(ctx context.Context, entities tg.Entities, update *tg.UpdateMessageReactions) error {
			n.peers.Observe(entities)
			frame, ok := n.reactions.messageReactions(ctx, entities, update)
			return n.send(ctx, frame, ok)
		})
		dispatcher.OnBotMessageReaction(func(ctx context.Context, entities tg.Entities, update *tg.UpdateBotMessageReaction) error {
			n.peers.Observe(entities)
			frame, ok := n.reactions.botReaction(ctx, entities, update)
			return n.send(ctx, frame, ok)
		})
	}
	// Only channel/supergroup deletes carry their peer; plain UpdateDeleteMessages
	// omits the chat, so a deleted DM/basic-group message cannot be resolved and
	// is not mirrored.
	dispatcher.OnDeleteChannelMessages(func(ctx context.Context, _ tg.Entities, update *tg.UpdateDeleteChannelMessages) error {
		return n.handleDelete(ctx, fmt.Sprintf("telegram:channel:%d", update.ChannelID), update.Messages)
	})
	return dispatcher
}

// send passes a frame to the core, when there is one.
func (n Normalizer) send(ctx context.Context, frame *wuphfpb.ClientFrame, ok bool) error {
	if !ok {
		return nil
	}
	select {
	case n.inputs <- frame:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// handleDelete reports each revoked message so the core can mirror the deletion
// to the other copies.
func (n Normalizer) handleDelete(ctx context.Context, chatID string, ids []int) error {
	for _, id := range ids {
		frame := &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_MessageDelete{MessageDelete: &wuphfpb.MessageDeleteEvent{
				ChatId:            chatID,
				PlatformMessageId: strconv.Itoa(id),
			}},
		}
		select {
		case n.inputs <- frame:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	return nil
}

func (n Normalizer) handle(ctx context.Context, entities tg.Entities, message tg.MessageClass) error {
	n.peers.Observe(entities)
	event, ok, err := n.normalize(ctx, entities, message)
	if err != nil {
		return err
	}
	if !ok {
		return nil
	}
	select {
	case n.inputs <- &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageCreate{MessageCreate: event},
	}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (n Normalizer) handleEdit(ctx context.Context, entities tg.Entities, message tg.MessageClass) error {
	n.peers.Observe(entities)
	event, ok := n.normalizeEdit(message)
	if !ok {
		return nil
	}
	select {
	case n.inputs <- &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageEdit{MessageEdit: event},
	}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// normalizeEdit reports an inbound content revision. Telegram also emits edit
// updates for non-content changes (view counts, reactions); the core suppresses
// those by comparing text, so this only needs to drop our own outbound echoes.
func (n Normalizer) normalizeEdit(class tg.MessageClass) (*wuphfpb.MessageEditEvent, bool) {
	message, ok := class.(*tg.Message)
	if !ok {
		return nil, false
	}
	if n.taken.has(formatPeer(message.PeerID), message.ID) {
		return nil, false
	}
	if message.Out {
		return nil, false
	}
	event := &wuphfpb.MessageEditEvent{
		ChatId:            formatPeer(message.PeerID),
		PlatformMessageId: strconv.Itoa(message.ID),
	}
	if topicID, ok := topicRootID(message); ok {
		thread := strconv.Itoa(topicID)
		event.ThreadId = &thread
	}
	if text := message.Message; text != "" {
		event.Text = &text
	}
	return event, true
}

func (n Normalizer) normalize(
	ctx context.Context,
	entities tg.Entities,
	class tg.MessageClass,
) (*wuphfpb.MessageCreateEvent, bool, error) {
	message, ok := class.(*tg.Message)
	if !ok {
		return nil, false, nil
	}
	// The panel goes first: a user session's owner types the command from
	// the session's own account, so it would otherwise be dropped as Out.
	if n.panel != nil && n.panel.Take(ctx, entities, message) {
		n.taken.add(formatPeer(message.PeerID), message.ID)
		return nil, false, nil
	}
	if message.Out {
		return nil, false, nil
	}
	var attachments []*wuphfpb.Attachment
	if media, hasMedia := message.GetMedia(); hasMedia {
		var supported bool
		var err error
		attachments, supported, err = n.media.Inbound(ctx, media)
		if err != nil {
			return nil, false, err
		}
		if !supported {
			return nil, false, nil
		}
	}
	if message.Message == "" && len(attachments) == 0 {
		return nil, false, nil
	}

	event := &wuphfpb.MessageCreateEvent{
		ChatId:            formatPeer(message.PeerID),
		PlatformMessageId: strconv.Itoa(message.ID),
		Sender:            senderID(message),
		Attachments:       attachments,
		TimestampMs:       int64(message.Date) * 1000,
	}
	if title := chatTitle(entities, message.PeerID); title != "" {
		event.ChatTitle = &title
	}
	if from, ok := message.GetFromID(); ok {
		if name := chatTitle(entities, from); name != "" {
			event.SenderName = &name
		}
		// The sender as owner_ids name users, so the core can tell the
		// owner's messages (a lone-emoji reply is their reaction, plan 081).
		if user, ok := from.(*tg.PeerUser); ok {
			account := reactorID(user)
			event.SenderAccount = &account
		}
	}
	if topicID, ok := topicRootID(message); ok {
		thread := strconv.Itoa(topicID)
		event.ThreadId = &thread
	}
	if text := message.Message; text != "" {
		event.Text = &text
	}
	if replyID, ok := replyMessageID(message); ok {
		reply := strconv.Itoa(replyID)
		event.ReplyToPlatformMessageId = &reply
		event.ReplyToChatId = replyChatID(message)
	}
	return event, true, nil
}

func senderID(message *tg.Message) string {
	if sender, ok := message.GetFromID(); ok {
		return formatPeer(sender)
	}
	return formatPeer(message.PeerID)
}

// topicRootID returns the forum topic a message belongs to, when any. Telegram
// threads a topic message by replying to the topic's root service message.
func topicRootID(message *tg.Message) (int, bool) {
	reply, ok := message.GetReplyTo()
	if !ok {
		return 0, false
	}
	return topicOf(reply)
}

// topicOf is the forum topic a reply header threads into, when any.
func topicOf(reply tg.MessageReplyHeaderClass) (int, bool) {
	header, ok := reply.(*tg.MessageReplyHeader)
	if !ok || !header.ForumTopic {
		return 0, false
	}
	if topID, ok := header.GetReplyToTopID(); ok {
		return topID, true
	}
	if rootID, ok := header.GetReplyToMsgID(); ok {
		return rootID, true
	}
	return 0, false
}

// replyChatID is the chat a reply quotes from, when it is another chat than
// the reply's own ("Reply in another chat", plan 083); nil for the same chat,
// whatever topic the quoted message is in. Without it the core would look the
// quoted id up in this chat, where it names some other message.
func replyChatID(message *tg.Message) *string {
	reply, ok := message.GetReplyTo()
	if !ok {
		return nil
	}
	header, ok := reply.(*tg.MessageReplyHeader)
	if !ok {
		return nil
	}
	peer, ok := header.GetReplyToPeerID()
	if !ok {
		return nil
	}
	chat := formatPeer(peer)
	if chat == formatPeer(message.PeerID) {
		return nil
	}
	return &chat
}

// replyMessageID returns the message a reply quotes, excluding the topic root
// (which is structural threading, not a user reply).
func replyMessageID(message *tg.Message) (int, bool) {
	reply, ok := message.GetReplyTo()
	if !ok {
		return 0, false
	}
	header, ok := reply.(*tg.MessageReplyHeader)
	if !ok {
		return 0, false
	}
	replyID, ok := header.GetReplyToMsgID()
	if !ok {
		return 0, false
	}
	if topID, hasTop := header.GetReplyToTopID(); hasTop && topID == replyID {
		return 0, false
	}
	if header.ForumTopic {
		if _, hasTop := header.GetReplyToTopID(); !hasTop {
			// reply_to_msg_id is the topic root itself, not a quoted message.
			return 0, false
		}
	}
	return replyID, true
}

// takenMessageTTL is how long the ids of taken messages are kept, so that
// their edits are not mirrored either.
const takenMessageTTL = time.Hour

// takenMessages remembers the messages the panel took, by chat and id, for a
// while. A nil *takenMessages remembers nothing.
type takenMessages struct {
	ttl time.Duration
	now func() time.Time

	mu       sync.Mutex
	expiries map[takenKey]time.Time
}

type takenKey struct {
	chat string
	id   int
}

func newTakenMessages(ttl time.Duration) *takenMessages {
	return &takenMessages{ttl: ttl, now: time.Now, expiries: map[takenKey]time.Time{}}
}

func (t *takenMessages) add(chat string, id int) {
	if t == nil {
		return
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	now := t.now()
	for key, expires := range t.expiries {
		if !now.Before(expires) {
			delete(t.expiries, key)
		}
	}
	t.expiries[takenKey{chat: chat, id: id}] = now.Add(t.ttl)
}

func (t *takenMessages) has(chat string, id int) bool {
	if t == nil {
		return false
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	expires, ok := t.expiries[takenKey{chat: chat, id: id}]
	return ok && t.now().Before(expires)
}
