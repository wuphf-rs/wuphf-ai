package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"path"
	"unicode/utf8"

	"github.com/akshettrj/wuphf/connectors/mediaconv"
	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	coreclient "github.com/akshettrj/wuphf/connectors/whatsapp/internal/core"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"
)

type SendClient interface {
	SendMessage(context.Context, types.JID, *waE2E.Message, ...whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error)
	Upload(context.Context, []byte, whatsmeow.MediaType) (whatsmeow.UploadResponse, error)
	BuildEdit(types.JID, types.MessageID, *waE2E.Message) *waE2E.Message
	BuildRevoke(chat, sender types.JID, id types.MessageID) *waE2E.Message
}

type Sender struct {
	client      SendClient
	media       MediaBroker
	ownJID      types.JID
	stickerMeta config.StickerMetadata
	// rendering is this connector's own policy for how mirrored copies are
	// rendered; the core dispatches content only.
	rendering config.RenderingOptions
}

func NewSender(client SendClient, media MediaBroker, ownJID types.JID, stickerMeta config.StickerMetadata, rendering config.RenderingOptions) Sender {
	return Sender{client: client, media: media, ownJID: ownJID, stickerMeta: stickerMeta, rendering: rendering}
}

// SendMessage delivers a dispatch to the target WhatsApp chat. WhatsApp has no
// threads, so the dispatch never carries a thread target and the returned
// threadID is always empty.
func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, string, error) {
	chat, err := types.ParseJID(dispatch.GetTargetChatId())
	if err != nil {
		return "", "", coreclient.Permanent(fmt.Errorf("parse WhatsApp target chat %q: %w", dispatch.GetTargetChatId(), err))
	}
	if len(dispatch.Attachments) > 1 {
		return "", "", coreclient.Permanent(errors.New("multiple attachment dispatch is not implemented"))
	}
	contextInfo, err := s.quotedContext(dispatch)
	if err != nil {
		return "", "", err
	}
	var message *waE2E.Message
	switch {
	case len(dispatch.Attachments) > 0:
		message, err = s.attachmentMessage(ctx, dispatch.Attachments[0], dispatch.GetText(), s.rendering.AttachmentsAsFiles)
		if err != nil {
			return "", "", err
		}
	case textExceedsLimit(dispatch.GetText(), s.rendering.MaxMessageLength):
		message, err = s.textFileMessage(ctx, dispatch.GetText())
		if err != nil {
			return "", "", err
		}
	default:
		message = textMessage(dispatch.GetText())
	}
	message = withContext(message, contextInfo)
	response, err := s.client.SendMessage(ctx, chat, message)
	if err != nil {
		return "", "", fmt.Errorf("send WhatsApp message: %w", err)
	}
	return string(response.ID), "", nil
}

// EditMessage revises an already-delivered copy in place. WhatsApp keeps the
// original message id across an edit, so the target id is returned unchanged for
// the acknowledgement.
func (s Sender) EditMessage(ctx context.Context, dispatch *wuphfpb.MessageEditDispatch) (string, error) {
	chat, err := types.ParseJID(dispatch.GetTargetChatId())
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("parse WhatsApp target chat %q: %w", dispatch.GetTargetChatId(), err))
	}
	edit := s.client.BuildEdit(chat, types.MessageID(dispatch.GetTargetPlatformMessageId()), textMessage(dispatch.GetText()))
	if _, err := s.client.SendMessage(ctx, chat, edit); err != nil {
		return "", fmt.Errorf("edit WhatsApp message: %w", err)
	}
	return dispatch.GetTargetPlatformMessageId(), nil
}

// DeleteMessage revokes an already-delivered copy. WhatsApp keeps the original
// message id after a revoke, so the target id is returned unchanged for the
// acknowledgement. The empty sender JID revokes our own (bridge-authored) copy.
func (s Sender) DeleteMessage(ctx context.Context, dispatch *wuphfpb.MessageDeleteDispatch) (string, error) {
	chat, err := types.ParseJID(dispatch.GetTargetChatId())
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("parse WhatsApp target chat %q: %w", dispatch.GetTargetChatId(), err))
	}
	revoke := s.client.BuildRevoke(chat, types.EmptyJID, types.MessageID(dispatch.GetTargetPlatformMessageId()))
	if _, err := s.client.SendMessage(ctx, chat, revoke); err != nil {
		return "", fmt.Errorf("delete WhatsApp message: %w", err)
	}
	return dispatch.GetTargetPlatformMessageId(), nil
}

// quotedContext builds the WhatsApp quote metadata for a reply dispatch. The
// quoted copy at this endpoint is always bridge-authored, since the router never
// dispatches a message back to its origin endpoint, so the quoted participant is
// our own account.
func (s Sender) quotedContext(dispatch *wuphfpb.MessageCreateDispatch) (*waE2E.ContextInfo, error) {
	reply := dispatch.GetReplyContext()
	if reply == nil {
		if dispatch.GetReplyToPlatformMessageId() != "" {
			return nil, coreclient.Permanent(errors.New("outbound WhatsApp reply requires quoted-message context"))
		}
		return nil, nil
	}
	if s.ownJID.IsEmpty() {
		return nil, coreclient.Permanent(errors.New("outbound WhatsApp reply requires a paired account JID"))
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
	asFile bool,
) (*waE2E.Message, error) {
	bytes, err := s.media.Download(ctx, attachment.MediaId)
	if err != nil {
		return nil, fmt.Errorf("download media from core: %w", err)
	}
	mimeType := attachment.GetMimeType()
	// The delivery policy can force every attachment to a document regardless of
	// its origin kind.
	kind := attachment.Kind
	if asFile {
		kind = wuphfpb.Attachment_KIND_FILE
	}
	switch kind {
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
		return s.documentMessage(ctx, bytes, mimeType, attachment.GetFileName(), caption)
	case wuphfpb.Attachment_KIND_VIDEO:
		uploaded, err := s.client.Upload(ctx, bytes, whatsmeow.MediaVideo)
		if err != nil {
			return nil, fmt.Errorf("upload WhatsApp video: %w", err)
		}
		return &waE2E.Message{VideoMessage: &waE2E.VideoMessage{
			Caption:  optionalProtoString(caption),
			Mimetype: optionalProtoString(mimeType),
			// A WhatsApp GIF is an MP4 with this flag set, so an animated video
			// (a GIF on the source) is marked to loop and play without sound.
			GifPlayback:   proto.Bool(attachment.GetAnimated()),
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
		return s.stickerMessage(ctx, attachment, bytes, mimeType)
	default:
		return nil, coreclient.Permanent(fmt.Errorf("unsupported outbound attachment kind %s", attachment.Kind))
	}
}

// stickerMessage delivers a sticker, preserving animation. WhatsApp stickers are
// WebP (animated stickers are animated WebP), so an animated sticker arriving in
// another format — chiefly a Telegram WebM video sticker — is transcoded to
// animated WebP first. A static sticker (already WebP) is uploaded unchanged.
// When the transcode cannot run (a Lottie .tgs has no raster form, or the tools
// are missing) the original is delivered as a file so the message is not lost.
func (s Sender) stickerMessage(
	ctx context.Context,
	attachment *wuphfpb.Attachment,
	data []byte,
	mimeType string,
) (*waE2E.Message, error) {
	if attachment.GetAnimated() && mimeType != "image/webp" {
		converted, err := animatedStickerWebP(ctx, attachment, data, mimeType)
		if err != nil {
			return s.documentMessage(ctx, data, mimeType, stickerFileName(attachment), "")
		}
		data, mimeType = converted, "image/webp"
	}
	if mimeType == "image/webp" {
		data = applyStickerMetadata(ctx, data, s.stickerMeta)
	}
	uploaded, err := s.client.Upload(ctx, data, whatsmeow.MediaImage)
	if err != nil {
		return nil, fmt.Errorf("upload WhatsApp sticker: %w", err)
	}
	return &waE2E.Message{StickerMessage: &waE2E.StickerMessage{
		Mimetype:      optionalProtoString(mimeType),
		IsAnimated:    proto.Bool(attachment.GetAnimated()),
		URL:           &uploaded.URL,
		DirectPath:    &uploaded.DirectPath,
		MediaKey:      uploaded.MediaKey,
		FileEncSHA256: uploaded.FileEncSHA256,
		FileSHA256:    uploaded.FileSHA256,
		FileLength:    &uploaded.FileLength,
	}}, nil
}

// animatedStickerWebP converts an animated sticker from another platform into the
// animated WebP a WhatsApp sticker needs, dispatching on the source format: a
// Telegram Lottie sticker (.tgs) is rasterized by a vector renderer, while any
// video sticker (chiefly a Telegram WebM) is transcoded by ffmpeg.
func animatedStickerWebP(
	ctx context.Context,
	attachment *wuphfpb.Attachment,
	data []byte,
	mimeType string,
) ([]byte, error) {
	if mimeType == "application/x-tgsticker" || stickerSourceExt(attachment) == ".tgs" {
		return tgsToAnimatedWebP(data)
	}
	return mediaconv.ToAnimatedWebP(ctx, data, stickerSourceExt(attachment))
}

// documentMessage uploads bytes as a WhatsApp document, the generic file form
// used both for plain files and as the fallback for media that cannot be sent in
// a richer kind.
func (s Sender) documentMessage(
	ctx context.Context,
	data []byte,
	mimeType, fileName, caption string,
) (*waE2E.Message, error) {
	uploaded, err := s.client.Upload(ctx, data, whatsmeow.MediaDocument)
	if err != nil {
		return nil, fmt.Errorf("upload WhatsApp file: %w", err)
	}
	return &waE2E.Message{DocumentMessage: &waE2E.DocumentMessage{
		Caption:       optionalProtoString(caption),
		Mimetype:      optionalProtoString(mimeType),
		FileName:      optionalProtoString(fileName),
		URL:           &uploaded.URL,
		DirectPath:    &uploaded.DirectPath,
		MediaKey:      uploaded.MediaKey,
		FileEncSHA256: uploaded.FileEncSHA256,
		FileSHA256:    uploaded.FileSHA256,
		FileLength:    &uploaded.FileLength,
	}}, nil
}

// stickerSourceExt picks the container extension for a sticker's bytes so the
// transcoder can demux it, derived from the mime type with a filename fallback.
func stickerSourceExt(attachment *wuphfpb.Attachment) string {
	switch attachment.GetMimeType() {
	case "video/webm":
		return ".webm"
	case "application/x-tgsticker":
		return ".tgs"
	}
	if ext := path.Ext(attachment.GetFileName()); ext != "" {
		return ext
	}
	return ".bin"
}

// stickerFileName names the fallback document for a sticker that could not be
// converted, preserving any source name.
func stickerFileName(attachment *wuphfpb.Attachment) string {
	if name := attachment.GetFileName(); name != "" {
		return name
	}
	return "sticker" + stickerSourceExt(attachment)
}

func textMessage(text string) *waE2E.Message {
	return &waE2E.Message{Conversation: proto.String(text)}
}

// textExceedsLimit reports whether the text is longer than the configured
// maximum, measured in Unicode code points. A zero limit means unbounded.
func textExceedsLimit(text string, limit uint32) bool {
	if limit == 0 {
		return false
	}
	return utf8.RuneCountInString(text) > int(limit)
}

// textFileMessage delivers an over-length message as a plain-text document so
// the platform's per-message length cap does not truncate or reject it.
func (s Sender) textFileMessage(ctx context.Context, text string) (*waE2E.Message, error) {
	uploaded, err := s.client.Upload(ctx, []byte(text), whatsmeow.MediaDocument)
	if err != nil {
		return nil, fmt.Errorf("upload WhatsApp text file: %w", err)
	}
	return &waE2E.Message{DocumentMessage: &waE2E.DocumentMessage{
		Mimetype:      optionalProtoString("text/plain"),
		FileName:      optionalProtoString("message.txt"),
		URL:           &uploaded.URL,
		DirectPath:    &uploaded.DirectPath,
		MediaKey:      uploaded.MediaKey,
		FileEncSHA256: uploaded.FileEncSHA256,
		FileSHA256:    uploaded.FileSHA256,
		FileLength:    &uploaded.FileLength,
	}}, nil
}

func optionalProtoString(value string) *string {
	if value == "" {
		return nil
	}
	return proto.String(value)
}
