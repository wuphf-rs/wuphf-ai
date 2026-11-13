package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"path"
	"unicode/utf8"

	coreclient "github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/shared/settings"
	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
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

// StickerConverter converts stored media to another format (in production,
// functions.MediaConverter calling media.convert@1 on the core).
type StickerConverter interface {
	Convert(ctx context.Context, mediaID, sourceMime, sourceName, targetMime string) ([]byte, error)
}

type Sender struct {
	client    SendClient
	media     MediaBroker
	converter StickerConverter
	// addressing translates the canonical addresses dispatches carry into the
	// form each chat is addressed in on WhatsApp.
	addressing Addressing
	// runtime holds this connector's own policy for how mirrored copies are
	// rendered (the core dispatches content only) and the sticker pack
	// metadata. It is a runtime setting: each dispatch reads it once.
	runtime *settings.Live[config.Runtime]
	// pinned is the value one dispatch read from runtime, so every part of
	// the dispatch sees the same settings (see pin).
	pinned *config.Runtime
	// timers decides the disappearing timer of a send the core dispatched
	// without one. Nil when the connector does not declare
	// supports_disappearing_timers: sends then carry only what the core sent.
	timers *timerSource
}

func NewSender(client SendClient, media MediaBroker, converter StickerConverter, addressing Addressing, runtime *settings.Live[config.Runtime]) Sender {
	return Sender{client: client, media: media, converter: converter, addressing: addressing, runtime: runtime}
}

// WithTimers returns the sender with a fallback for sends whose dispatch
// carries no disappearing timer.
func (s Sender) WithTimers(timers *timerSource) Sender {
	s.timers = timers
	return s
}

// disappearing is the timer a send to chat carries: the dispatch's, or what
// the fallback decides when it has none.
func (s Sender) disappearing(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch, chat types.JID) *wuphfpb.DisappearingTimer {
	timer := dispatch.GetDisappearing()
	if s.timers != nil {
		timer = s.timers.For(ctx, dispatch.GetTargetChatId(), chat, timer)
	}
	return timer
}

// settings are the runtime settings: the dispatch's pinned value, else the
// current one; the zero value when the sender has none.
func (s Sender) settings() config.Runtime {
	switch {
	case s.pinned != nil:
		return *s.pinned
	case s.runtime == nil:
		return config.Runtime{}
	default:
		return s.runtime.Load()
	}
}

// pin returns a copy of the sender bound to the settings as they are now, for
// one dispatch: a change pushed meanwhile applies to the next one.
func (s Sender) pin() Sender {
	current := s.settings()
	s.pinned = &current
	return s
}

// SendMessage delivers a dispatch to the target WhatsApp chat. WhatsApp has no
// threads, so the dispatch never carries a thread target and the returned
// threadID is always empty.
func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, string, error) {
	s = s.pin()
	chat, mode, err := s.target(ctx, dispatch.GetTargetChatId())
	if err != nil {
		return "", "", err
	}
	// A system note (an info card, a change to the chat) is the bridge
	// speaking: it quotes nothing.
	var contextInfo *waE2E.ContextInfo
	if !dispatch.GetSystemNote() {
		contextInfo, err = s.quotedContext(ctx, dispatch, mode)
		if err != nil {
			return "", "", err
		}
	}
	// Every part of a content send carries the chat's timer, so it disappears
	// like the rest of the chat instead of showing "will not disappear".
	timer := s.disappearing(ctx, dispatch, chat)
	if len(dispatch.Attachments) > 1 {
		id, err := s.sendAttachments(ctx, chat, dispatch, contextInfo, timer)
		return id, "", err
	}
	var message *waE2E.Message
	switch {
	case len(dispatch.Attachments) > 0:
		message, err = s.attachmentMessage(ctx, dispatch.Attachments[0], dispatch.GetText(), s.settings().Rendering.AttachmentsAsFiles)
		if err != nil {
			return "", "", err
		}
	case textExceedsLimit(dispatch.GetText(), s.settings().Rendering.MaxMessageLength):
		message, err = s.textFileMessage(ctx, dispatch.GetText())
		if err != nil {
			return "", "", err
		}
	default:
		message = textMessage(dispatch.GetText())
	}
	message = withDisappearing(withContext(message, contextInfo), timer)
	response, err := s.client.SendMessage(ctx, chat, message)
	if err != nil {
		return "", "", fmt.Errorf("send WhatsApp message: %w", err)
	}
	return string(response.ID), "", nil
}

// sendAttachments delivers a multi-attachment dispatch. WhatsApp carries one
// media per message and whatsmeow exposes no album primitive, so the group goes
// out as consecutive messages: the first carries the dispatch text as its
// caption and the quoted context, the rest are bare media. Every one carries
// the chat's disappearing timer. The first message's
// id is returned as the copy, so replies and edits against this message resolve
// to the head of the group.
//
// A failure partway through is reported as permanent. The earlier messages are
// already delivered and cannot be recalled, so letting the core redeliver would
// duplicate them; dead-lettering instead leaves the partial group in place and
// surfaces the failure once.
func (s Sender) sendAttachments(
	ctx context.Context,
	chat types.JID,
	dispatch *wuphfpb.MessageCreateDispatch,
	contextInfo *waE2E.ContextInfo,
	timer *wuphfpb.DisappearingTimer,
) (string, error) {
	var first string
	for i, attachment := range dispatch.Attachments {
		caption := ""
		if i == 0 {
			caption = dispatch.GetText()
		}
		message, err := s.attachmentMessage(ctx, attachment, caption, s.settings().Rendering.AttachmentsAsFiles)
		if err != nil {
			return "", attachmentGroupError(err, i)
		}
		if i == 0 {
			message = withContext(message, contextInfo)
		}
		message = withDisappearing(message, timer)
		response, err := s.client.SendMessage(ctx, chat, message)
		if err != nil {
			return "", attachmentGroupError(fmt.Errorf("send WhatsApp message: %w", err), i)
		}
		if i == 0 {
			first = string(response.ID)
		}
	}
	return first, nil
}

// attachmentGroupError marks a failure that leaves a partly-sent attachment
// group permanent, so the dispatch is dead-lettered rather than redelivered
// into duplicates. A failure on the first attachment has sent nothing yet and
// keeps whatever retry semantics the underlying error carries.
func attachmentGroupError(err error, index int) error {
	if index == 0 {
		return err
	}
	return coreclient.Permanent(fmt.Errorf(
		"attachment %d of a multi-attachment message failed after %d were sent: %w",
		index+1, index, err,
	))
}

// EditMessage revises an already-delivered copy in place. WhatsApp keeps the
// original message id across an edit, so the target id is returned unchanged for
// the acknowledgement.
func (s Sender) EditMessage(ctx context.Context, dispatch *wuphfpb.MessageEditDispatch) (string, error) {
	chat, _, err := s.target(ctx, dispatch.GetTargetChatId())
	if err != nil {
		return "", err
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
	chat, _, err := s.target(ctx, dispatch.GetTargetChatId())
	if err != nil {
		return "", err
	}
	revoke := s.client.BuildRevoke(chat, types.EmptyJID, types.MessageID(dispatch.GetTargetPlatformMessageId()))
	if _, err := s.client.SendMessage(ctx, chat, revoke); err != nil {
		return "", fmt.Errorf("delete WhatsApp message: %w", err)
	}
	return dispatch.GetTargetPlatformMessageId(), nil
}

// quotedContext builds the WhatsApp quote metadata for a reply dispatch. The
// quoted copy is not necessarily one the bridge wrote: the router only skips the
// *replying* message's origin endpoint, so a reply arriving from another
// platform can quote a message this chat sent us. The dispatch says which it is.
func (s Sender) quotedContext(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch, mode types.AddressingMode) (*waE2E.ContextInfo, error) {
	reply := dispatch.GetReplyContext()
	if reply == nil {
		if dispatch.GetReplyToPlatformMessageId() != "" {
			return nil, coreclient.Permanent(errors.New("outbound WhatsApp reply requires quoted-message context"))
		}
		return nil, nil
	}
	participant, err := s.quotedAuthor(ctx, reply, mode)
	if err != nil {
		return nil, err
	}
	return &waE2E.ContextInfo{
		StanzaID:      proto.String(reply.GetPlatformMessageId()),
		Participant:   proto.String(participant),
		QuotedMessage: quotedBody(reply),
	}, nil
}

// quotedAuthor resolves the JID WhatsApp is told wrote the quoted message.
// WhatsApp links a quote by (StanzaID, Participant) together, so naming the
// wrong author leaves the reply unlinked in clients.
//
// A copy the bridge sent into this chat was written by the paired account. A
// copy the bridge received here came from WhatsApp itself, so the reply context
// already carries this platform's own ids for its author — the group
// participant when there is one, the chat peer otherwise. The normalizer reported
// it in canonical form, which need not be the form the chat is addressed in, so it
// is translated into the chat's form (mode); so is the paired account's own
// address, since WhatsApp keys our messages in a LID-addressed chat by our LID.
func (s Sender) quotedAuthor(ctx context.Context, reply *wuphfpb.ReplyContext, mode types.AddressingMode) (string, error) {
	if !reply.GetAuthoredByBridge() {
		author := reply.GetParticipant()
		if author == "" {
			author = reply.GetSender()
		}
		if author != "" {
			jid, err := types.ParseJID(author)
			if err != nil {
				return author, nil
			}
			return s.addressing.InMode(ctx, jid, mode).String(), nil
		}
	}
	own := s.addressing.Own(mode)
	if own.IsEmpty() {
		return "", coreclient.Permanent(errors.New("outbound WhatsApp reply requires a paired account JID"))
	}
	return own.String(), nil
}

// target parses a dispatch's canonical chat id and returns the chat in the form
// WhatsApp addresses it in, with that form for the frame's other addresses.
func (s Sender) target(ctx context.Context, chatID string) (types.JID, types.AddressingMode, error) {
	chat, err := types.ParseJID(chatID)
	if err != nil {
		return types.EmptyJID, "", coreclient.Permanent(fmt.Errorf("parse WhatsApp target chat %q: %w", chatID, err))
	}
	mode := s.addressing.Mode(ctx, chat)
	return s.addressing.InMode(ctx, chat, mode), mode, nil
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
// The transcode runs on the core's media.convert provider; when no provider is
// available or the conversion fails, the original is delivered as a file so the
// message is not lost.
func (s Sender) stickerMessage(
	ctx context.Context,
	attachment *wuphfpb.Attachment,
	data []byte,
	mimeType string,
) (*waE2E.Message, error) {
	if attachment.GetAnimated() && mimeType != "image/webp" {
		converted, err := s.animatedStickerWebP(ctx, attachment, mimeType)
		if err != nil {
			return s.documentMessage(ctx, data, mimeType, stickerFileName(attachment), "")
		}
		data, mimeType = converted, "image/webp"
	}
	if mimeType == "image/webp" {
		data = applyStickerMetadata(ctx, data, s.settings().StickerMetadata())
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
// animated WebP a WhatsApp sticker needs, through media.convert. The provider
// picks the conversion from the source format: a Telegram Lottie sticker (.tgs)
// is rasterized, while a video sticker (chiefly a Telegram WebM) is transcoded.
// mimeType is the type stickerMessage resolved for the source, which the
// provider dispatches on.
func (s Sender) animatedStickerWebP(ctx context.Context, attachment *wuphfpb.Attachment, mimeType string) ([]byte, error) {
	return s.converter.Convert(ctx, attachment.GetMediaId(), mimeType, attachment.GetFileName(), "image/webp")
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
