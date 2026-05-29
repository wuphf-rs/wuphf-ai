package telegramconnector

import (
	"context"
	"strconv"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/tg"
)

type Normalizer struct {
	targets TargetRegistry
	inputs  chan<- *wuphfpb.ClientFrame
	media   MediaCodec
}

func NewNormalizer(
	targets TargetRegistry,
	inputs chan<- *wuphfpb.ClientFrame,
	media MediaCodec,
) Normalizer {
	return Normalizer{targets: targets, inputs: inputs, media: media}
}

func (n Normalizer) Dispatcher() tg.UpdateDispatcher {
	dispatcher := tg.NewUpdateDispatcher()
	dispatcher.OnNewMessage(func(ctx context.Context, _ tg.Entities, update *tg.UpdateNewMessage) error {
		return n.handle(ctx, update.Message)
	})
	dispatcher.OnNewChannelMessage(func(ctx context.Context, _ tg.Entities, update *tg.UpdateNewChannelMessage) error {
		return n.handle(ctx, update.Message)
	})
	return dispatcher
}

func (n Normalizer) handle(ctx context.Context, message tg.MessageClass) error {
	event, ok, err := n.normalize(ctx, message)
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

func (n Normalizer) normalize(
	ctx context.Context,
	class tg.MessageClass,
) (*wuphfpb.MessageCreateEvent, bool, error) {
	message, ok := class.(*tg.Message)
	if !ok {
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

	targets := n.targets.Incoming(message.PeerID)
	for _, target := range targets {
		if !matchesTopic(message, target.TopicRootMessageID) {
			continue
		}
		text := message.Message
		event := &wuphfpb.MessageCreateEvent{
			OriginEndpointId:  target.EndpointID,
			GroupId:           target.GroupID,
			PlatformMessageId: strconv.Itoa(message.ID),
			Sender:            senderID(message),
			Attachments:       attachments,
			TimestampMs:       int64(message.Date) * 1000,
		}
		if text != "" {
			event.Text = &text
		}
		if replyID, ok := logicalReplyID(message, target.TopicRootMessageID); ok {
			reply := strconv.Itoa(replyID)
			event.ReplyToPlatformMessageId = &reply
		}
		return event, true, nil
	}
	return nil, false, nil
}

func senderID(message *tg.Message) string {
	if sender, ok := message.GetFromID(); ok {
		return formatPeer(sender)
	}
	return formatPeer(message.PeerID)
}

func matchesTopic(message *tg.Message, topicRootMessageID int) bool {
	if topicRootMessageID == 0 {
		return true
	}
	reply, ok := message.GetReplyTo()
	if !ok {
		return false
	}
	header, ok := reply.(*tg.MessageReplyHeader)
	if !ok {
		return false
	}
	if topID, ok := header.GetReplyToTopID(); ok {
		return topID == topicRootMessageID
	}
	replyID, ok := header.GetReplyToMsgID()
	return ok && replyID == topicRootMessageID
}

func logicalReplyID(message *tg.Message, topicRootMessageID int) (int, bool) {
	reply, ok := message.GetReplyTo()
	if !ok {
		return 0, false
	}
	header, ok := reply.(*tg.MessageReplyHeader)
	if !ok {
		return 0, false
	}
	replyID, ok := header.GetReplyToMsgID()
	if !ok || replyID == topicRootMessageID {
		return 0, false
	}
	return replyID, true
}
