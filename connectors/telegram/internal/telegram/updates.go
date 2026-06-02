package telegramconnector

import (
	"context"
	"fmt"
	"strconv"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/tg"
)

type Normalizer struct {
	peers  *PeerCache
	inputs chan<- *wuphfpb.ClientFrame
	media  MediaCodec
}

func NewNormalizer(
	peers *PeerCache,
	inputs chan<- *wuphfpb.ClientFrame,
	media MediaCodec,
) Normalizer {
	return Normalizer{peers: peers, inputs: inputs, media: media}
}

func (n Normalizer) Dispatcher() tg.UpdateDispatcher {
	dispatcher := tg.NewUpdateDispatcher()
	dispatcher.OnNewMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateNewMessage) error {
		return n.handle(ctx, entities, update.Message)
	})
	dispatcher.OnNewChannelMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateNewChannelMessage) error {
		return n.handle(ctx, entities, update.Message)
	})
	dispatcher.OnEditMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateEditMessage) error {
		return n.handleEdit(ctx, entities, update.Message)
	})
	dispatcher.OnEditChannelMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateEditChannelMessage) error {
		return n.handleEdit(ctx, entities, update.Message)
	})
	// Only channel/supergroup deletes carry their peer; plain UpdateDeleteMessages
	// omits the chat, so a deleted DM/basic-group message cannot be resolved and
	// is not mirrored.
	dispatcher.OnDeleteChannelMessages(func(ctx context.Context, _ tg.Entities, update *tg.UpdateDeleteChannelMessages) error {
		return n.handleDelete(ctx, fmt.Sprintf("telegram:channel:%d", update.ChannelID), update.Messages)
	})
	return dispatcher
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
