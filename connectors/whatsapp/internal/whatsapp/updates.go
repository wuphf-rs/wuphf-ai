package whatsappconnector

import (
	"context"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types/events"
)

type Normalizer struct {
	targets TargetRegistry
	inputs  chan<- *wuphfpb.ClientFrame
	media   MediaCodec
}

func NewNormalizer(targets TargetRegistry, inputs chan<- *wuphfpb.ClientFrame, media MediaCodec) Normalizer {
	return Normalizer{targets: targets, inputs: inputs, media: media}
}

func (n Normalizer) Handle(ctx context.Context, message *events.Message) error {
	event, ok, err := n.normalize(ctx, message)
	if err != nil || !ok {
		return err
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

func (n Normalizer) normalize(ctx context.Context, message *events.Message) (*wuphfpb.MessageCreateEvent, bool, error) {
	if message.IsEdit {
		return nil, false, nil
	}
	target, ok := n.targets.Incoming(message.Info.Chat)
	if !ok {
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
	event := &wuphfpb.MessageCreateEvent{
		OriginEndpointId:  target.EndpointID,
		GroupId:           target.GroupID,
		PlatformMessageId: string(message.Info.ID),
		Sender:            message.Info.Sender.String(),
		Attachments:       attachments,
		TimestampMs:       message.Info.Timestamp.UnixMilli(),
	}
	if message.Info.IsGroup {
		participant := message.Info.Sender.String()
		event.Participant = &participant
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
	default:
		return nil
	}
}

func hasAttachment(message *waE2E.Message) bool {
	return message.GetImageMessage() != nil || message.GetDocumentMessage() != nil
}
