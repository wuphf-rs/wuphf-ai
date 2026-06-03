package instagramconnector

import (
	"context"
	"strconv"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

type Normalizer struct {
	inputs chan<- *wuphfpb.ClientFrame
}

func NewNormalizer(inputs chan<- *wuphfpb.ClientFrame) Normalizer {
	return Normalizer{inputs: inputs}
}

// Handle normalizes the message rows in a lightspeed table delta and submits
// each as a create event. MVP scope is text only: rows with no text body (media,
// stickers, unsends) are skipped. The core owns echo suppression, so our own
// (including bridge-authored) messages are submitted unconditionally.
func (n Normalizer) Handle(ctx context.Context, tbl *table.LSTable) error {
	if tbl == nil {
		return nil
	}
	for _, message := range tbl.LSInsertMessage {
		event, ok := normalize(message)
		if !ok {
			continue
		}
		if err := n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_MessageCreate{MessageCreate: event},
		}); err != nil {
			return err
		}
	}
	return nil
}

func normalize(message *table.LSInsertMessage) (*wuphfpb.MessageCreateEvent, bool) {
	if message.IsUnsent || message.Text == "" || message.MessageId == "" {
		return nil, false
	}
	text := message.Text
	sender := strconv.FormatInt(message.SenderId, 10)
	event := &wuphfpb.MessageCreateEvent{
		ChatId:            strconv.FormatInt(message.ThreadKey, 10),
		PlatformMessageId: message.MessageId,
		Sender:            sender,
		TimestampMs:       message.TimestampMs,
		Text:              &text,
	}
	if message.ReplySourceId != "" {
		reply := message.ReplySourceId
		event.ReplyToPlatformMessageId = &reply
	}
	return event, true
}

func (n Normalizer) send(ctx context.Context, frame *wuphfpb.ClientFrame) error {
	select {
	case n.inputs <- frame:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
