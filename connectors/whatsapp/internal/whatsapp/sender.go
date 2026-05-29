package whatsappconnector

import (
	"context"
	"errors"
	"fmt"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"
)

type SendClient interface {
	SendMessage(context.Context, types.JID, *waE2E.Message, ...whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error)
	Upload(context.Context, []byte, whatsmeow.MediaType) (whatsmeow.UploadResponse, error)
}

type Sender struct {
	client  SendClient
	targets TargetRegistry
	media   MediaBroker
	ownJID  types.JID
}

func NewSender(client SendClient, targets TargetRegistry, media MediaBroker, ownJID types.JID) Sender {
	return Sender{client: client, targets: targets, media: media, ownJID: ownJID}
}

func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, error) {
	target, ok := s.targets.Outbound(dispatch.TargetEndpointId)
	if !ok {
		return "", fmt.Errorf("no WhatsApp target for endpoint %q", dispatch.TargetEndpointId)
	}
	if len(dispatch.Attachments) > 1 {
		return "", errors.New("multiple attachment dispatch is not implemented")
	}
	contextInfo, err := s.quotedContext(dispatch)
	if err != nil {
		return "", err
	}
	var message *waE2E.Message
	if len(dispatch.Attachments) == 0 {
		message = textMessage(dispatch.GetText())
	} else {
		message, err = s.attachmentMessage(ctx, dispatch.Attachments[0], dispatch.GetText())
		if err != nil {
			return "", err
		}
	}
	message = withContext(message, contextInfo)
	response, err := s.client.SendMessage(ctx, target.Chat, message)
	if err != nil {
		return "", fmt.Errorf("send WhatsApp message: %w", err)
	}
	return string(response.ID), nil
}

// quotedContext builds the WhatsApp quote metadata for a reply dispatch. The
// quoted copy at this endpoint is always bridge-authored, since the router never
// dispatches a message back to its origin endpoint, so the quoted participant is
// our own account.
func (s Sender) quotedContext(dispatch *wuphfpb.MessageCreateDispatch) (*waE2E.ContextInfo, error) {
	reply := dispatch.GetReplyContext()
	if reply == nil {
		if dispatch.GetReplyToPlatformMessageId() != "" {
			return nil, errors.New("outbound WhatsApp reply requires quoted-message context")
		}
		return nil, nil
	}
	if s.ownJID.IsEmpty() {
		return nil, errors.New("outbound WhatsApp reply requires a paired account JID")
	}
	return &waE2E.ContextInfo{
		StanzaID:      proto.String(reply.GetPlatformMessageId()),
		Participant:   proto.String(s.ownJID.ToNonAD().String()),
		QuotedMessage: quotedBody(reply),
	}, nil
}

// quotedBody renders the parent message for the WhatsApp quote preview. The
// reply context only carries attachment metadata, not media keys, so a media
// parent is quoted as a minimal typed stub (kind plus caption) which clients
// render as a media reply rather than empty text.
func quotedBody(reply *wuphfpb.ReplyContext) *waE2E.Message {
	caption := optionalProtoString(reply.GetText())
	attachments := reply.GetAttachments()
	if len(attachments) == 0 {
		return &waE2E.Message{Conversation: proto.String(reply.GetText())}
	}
	attachment := attachments[0]
	mimeType := optionalProtoString(attachment.GetMimeType())
	switch attachment.Kind {
	case wuphfpb.Attachment_KIND_IMAGE:
		return &waE2E.Message{ImageMessage: &waE2E.ImageMessage{
			Caption:  caption,
			Mimetype: mimeType,
		}}
	case wuphfpb.Attachment_KIND_FILE:
		return &waE2E.Message{DocumentMessage: &waE2E.DocumentMessage{
			Caption:  caption,
			Mimetype: mimeType,
			FileName: optionalProtoString(attachment.GetFileName()),
		}}
	case wuphfpb.Attachment_KIND_VIDEO:
		return &waE2E.Message{VideoMessage: &waE2E.VideoMessage{
			Caption:  caption,
			Mimetype: mimeType,
		}}
	case wuphfpb.Attachment_KIND_AUDIO:
		return &waE2E.Message{AudioMessage: &waE2E.AudioMessage{
			Mimetype: mimeType,
		}}
	case wuphfpb.Attachment_KIND_STICKER:
		return &waE2E.Message{StickerMessage: &waE2E.StickerMessage{
			Mimetype: mimeType,
		}}
	default:
		return &waE2E.Message{Conversation: proto.String(reply.GetText())}
	}
}

func withContext(message *waE2E.Message, contextInfo *waE2E.ContextInfo) *waE2E.Message {
	if contextInfo == nil {
		return message
	}
	switch {
	case message.ImageMessage != nil:
		message.ImageMessage.ContextInfo = contextInfo
	case message.DocumentMessage != nil:
		message.DocumentMessage.ContextInfo = contextInfo
	case message.VideoMessage != nil:
		message.VideoMessage.ContextInfo = contextInfo
	case message.AudioMessage != nil:
		message.AudioMessage.ContextInfo = contextInfo
	case message.StickerMessage != nil:
		message.StickerMessage.ContextInfo = contextInfo
	case message.Conversation != nil:
		message = &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
			Text:        message.Conversation,
			ContextInfo: contextInfo,
		}}
	}
	return message
}

func (s Sender) attachmentMessage(
	ctx context.Context,
	attachment *wuphfpb.Attachment,
	caption string,
) (*waE2E.Message, error) {
	bytes, err := s.media.Download(ctx, attachment.MediaId)
	if err != nil {
		return nil, fmt.Errorf("download media from core: %w", err)
	}
	mimeType := attachment.GetMimeType()
	switch attachment.Kind {
	case wuphfpb.Attachment_KIND_IMAGE:
		uploaded, err := s.client.Upload(ctx, bytes, whatsmeow.MediaImage)
		if err != nil {
			return nil, fmt.Errorf("upload WhatsApp image: %w", err)
		}
		return &waE2E.Message{ImageMessage: &waE2E.ImageMessage{
			Caption:       optionalProtoString(caption),
			Mimetype:      optionalProtoString(mimeType),
			URL:           &uploaded.URL,
			DirectPath:    &uploaded.DirectPath,
			MediaKey:      uploaded.MediaKey,
			FileEncSHA256: uploaded.FileEncSHA256,
			FileSHA256:    uploaded.FileSHA256,
			FileLength:    &uploaded.FileLength,
		}}, nil
	case wuphfpb.Attachment_KIND_FILE:
		uploaded, err := s.client.Upload(ctx, bytes, whatsmeow.MediaDocument)
		if err != nil {
			return nil, fmt.Errorf("upload WhatsApp file: %w", err)
		}
		return &waE2E.Message{DocumentMessage: &waE2E.DocumentMessage{
			Caption:       optionalProtoString(caption),
			Mimetype:      optionalProtoString(mimeType),
			FileName:      optionalProtoString(attachment.GetFileName()),
			URL:           &uploaded.URL,
			DirectPath:    &uploaded.DirectPath,
			MediaKey:      uploaded.MediaKey,
			FileEncSHA256: uploaded.FileEncSHA256,
			FileSHA256:    uploaded.FileSHA256,
			FileLength:    &uploaded.FileLength,
		}}, nil
	case wuphfpb.Attachment_KIND_VIDEO:
		uploaded, err := s.client.Upload(ctx, bytes, whatsmeow.MediaVideo)
		if err != nil {
			return nil, fmt.Errorf("upload WhatsApp video: %w", err)
		}
		return &waE2E.Message{VideoMessage: &waE2E.VideoMessage{
			Caption:       optionalProtoString(caption),
			Mimetype:      optionalProtoString(mimeType),
			URL:           &uploaded.URL,
			DirectPath:    &uploaded.DirectPath,
			MediaKey:      uploaded.MediaKey,
			FileEncSHA256: uploaded.FileEncSHA256,
			FileSHA256:    uploaded.FileSHA256,
			FileLength:    &uploaded.FileLength,
		}}, nil
	case wuphfpb.Attachment_KIND_AUDIO:
		uploaded, err := s.client.Upload(ctx, bytes, whatsmeow.MediaAudio)
		if err != nil {
			return nil, fmt.Errorf("upload WhatsApp audio: %w", err)
		}
		return &waE2E.Message{AudioMessage: &waE2E.AudioMessage{
			Mimetype:      optionalProtoString(mimeType),
			URL:           &uploaded.URL,
			DirectPath:    &uploaded.DirectPath,
			MediaKey:      uploaded.MediaKey,
			FileEncSHA256: uploaded.FileEncSHA256,
			FileSHA256:    uploaded.FileSHA256,
			FileLength:    &uploaded.FileLength,
		}}, nil
	case wuphfpb.Attachment_KIND_STICKER:
		uploaded, err := s.client.Upload(ctx, bytes, whatsmeow.MediaImage)
		if err != nil {
			return nil, fmt.Errorf("upload WhatsApp sticker: %w", err)
		}
		return &waE2E.Message{StickerMessage: &waE2E.StickerMessage{
			Mimetype:      optionalProtoString(mimeType),
			URL:           &uploaded.URL,
			DirectPath:    &uploaded.DirectPath,
			MediaKey:      uploaded.MediaKey,
			FileEncSHA256: uploaded.FileEncSHA256,
			FileSHA256:    uploaded.FileSHA256,
			FileLength:    &uploaded.FileLength,
		}}, nil
	default:
		return nil, fmt.Errorf("unsupported outbound attachment kind %s", attachment.Kind)
	}
}

func textMessage(text string) *waE2E.Message {
	return &waE2E.Message{Conversation: proto.String(text)}
}

func optionalProtoString(value string) *string {
	if value == "" {
		return nil
	}
	return proto.String(value)
}
