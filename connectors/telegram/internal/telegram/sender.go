package telegramconnector

import (
	"context"
	"errors"
	"fmt"
	"strconv"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/telegram/message/styling"
	"github.com/gotd/td/tg"
)

type Sender struct {
	sender  *message.Sender
	targets TargetRegistry
	media   MediaBroker
}

func NewSender(api *tg.Client, targets TargetRegistry, media MediaBroker) Sender {
	return Sender{sender: message.NewSender(api), targets: targets, media: media}
}

func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, error) {
	target, ok := s.targets.Outbound(dispatch.TargetEndpointId)
	if !ok {
		return "", fmt.Errorf("no Telegram target for endpoint %q", dispatch.TargetEndpointId)
	}
	if len(dispatch.Attachments) > 1 {
		return "", errors.New("multiple attachment dispatch is not implemented")
	}

	builder := s.sender.Resolve(target.Peer).CloneBuilder()
	if nativeReplyID := replyTargetMessageID(dispatch); nativeReplyID != "" {
		replyID, err := strconv.Atoi(nativeReplyID)
		if err != nil {
			return "", fmt.Errorf("invalid Telegram reply message ID %q: %w", nativeReplyID, err)
		}
		builder.Reply(replyID)
	} else if target.TopicRootMessageID != 0 {
		builder.Reply(target.TopicRootMessageID)
	}

	var updates tg.UpdatesClass
	var err error
	if len(dispatch.Attachments) == 0 {
		updates, err = builder.Text(ctx, dispatch.GetText())
	} else {
		updates, err = s.sendAttachment(ctx, builder, dispatch.Attachments[0], dispatch.GetText())
	}
	if err != nil {
		return "", fmt.Errorf("send Telegram message: %w", err)
	}
	messageID, err := sentMessageID(updates)
	if err != nil {
		return "", err
	}
	return strconv.Itoa(messageID), nil
}

func (s Sender) sendAttachment(
	ctx context.Context,
	builder *message.Builder,
	attachment *wuphfpb.Attachment,
	caption string,
) (tg.UpdatesClass, error) {
	bytes, err := s.media.Download(ctx, attachment.MediaId)
	if err != nil {
		return nil, fmt.Errorf("download media from core: %w", err)
	}
	name := attachment.GetFileName()
	if name == "" {
		name = "attachment"
	}
	upload := builder.Upload(message.FromBytes(name, bytes))
	var captionOptions []message.StyledTextOption
	if caption != "" {
		captionOptions = []message.StyledTextOption{styling.Plain(caption)}
	}
	switch attachment.Kind {
	case wuphfpb.Attachment_KIND_IMAGE:
		return upload.Photo(ctx, captionOptions...)
	case wuphfpb.Attachment_KIND_VIDEO:
		return upload.Video(ctx, captionOptions...)
	case wuphfpb.Attachment_KIND_AUDIO:
		return upload.Audio(ctx, captionOptions...)
	case wuphfpb.Attachment_KIND_STICKER:
		return upload.Sticker(ctx, captionOptions...)
	case wuphfpb.Attachment_KIND_FILE:
		return upload.File(ctx, captionOptions...)
	default:
		return nil, fmt.Errorf("unsupported outbound attachment kind %s", attachment.Kind)
	}
}

func replyTargetMessageID(dispatch *wuphfpb.MessageCreateDispatch) string {
	if context := dispatch.GetReplyContext(); context != nil {
		return context.GetPlatformMessageId()
	}
	return dispatch.GetReplyToPlatformMessageId()
}

func sentMessageID(updates tg.UpdatesClass) (int, error) {
	switch update := updates.(type) {
	case *tg.UpdateShortSentMessage:
		return update.ID, nil
	case *tg.Updates:
		return messageIDFromUpdates(update.Updates)
	case *tg.UpdatesCombined:
		return messageIDFromUpdates(update.Updates)
	default:
		return 0, fmt.Errorf("unexpected Telegram send response %T", updates)
	}
}

func messageIDFromUpdates(updates []tg.UpdateClass) (int, error) {
	for _, update := range updates {
		switch created := update.(type) {
		case *tg.UpdateNewMessage:
			return created.Message.GetID(), nil
		case *tg.UpdateNewChannelMessage:
			return created.Message.GetID(), nil
		}
	}
	return 0, errors.New("Telegram send response contains no created message")
}
