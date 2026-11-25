package telegramconnector

import (
	"context"
	"errors"
	"fmt"
	"math/rand"
	"mime"
	"path"
	"strconv"
	"time"
	"unicode/utf16"

	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/telegram/message/markup"
	"github.com/gotd/td/telegram/message/styling"
	"github.com/gotd/td/tg"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/telegram/internal/config"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// StickerConverter converts stored media to another format (in production,
// functions.MediaConverter calling media.convert@1 on the core).
type StickerConverter interface {
	Convert(ctx context.Context, mediaID, sourceMime, sourceName, targetMime string) ([]byte, error)
}

type Sender struct {
	api       *tg.Client
	sender    *message.Sender
	peers     *PeerCache
	media     MediaBroker
	converter StickerConverter
	// bot is set for a bot session, which can attach inline keyboards; a user
	// session cannot.
	bot bool
	// runtime holds this connector's own policy for how mirrored copies are
	// rendered (the core dispatches content only). It is a runtime setting:
	// each dispatch reads it once.
	runtime *settings.Live[config.Runtime]
	// stickers sends a user session's labelled stickers through the helper
	// bot, when there is one (see sendSticker).
	stickers *InlineStickers
	// after runs f once d has passed (time.AfterFunc when nil): the refusal
	// note's self-deletion, replaced in tests.
	after func(d time.Duration, f func())
	// panel opens the settings panel when the core asks (plan 084); nil
	// without one.
	panel panelOpener
}

func NewSender(api *tg.Client, peers *PeerCache, media MediaBroker, converter StickerConverter, runtime *settings.Live[config.Runtime], bot bool) Sender {
	return Sender{
		api:       api,
		sender:    message.NewSender(api),
		peers:     peers,
		media:     media,
		converter: converter,
		bot:       bot,
		runtime:   runtime,
	}
}

// WithInlineStickers lets a user session label its stickers with a button,
// through the helper bot.
func (s Sender) WithInlineStickers(stickers *InlineStickers) Sender {
	s.stickers = stickers
	return s
}

// rendering is the current rendering policy; the zero value when the sender
// has no runtime settings (as in tests).
func (s Sender) rendering() config.RenderingOptions {
	if s.runtime == nil {
		return config.RenderingOptions{}
	}
	return s.runtime.Load().Rendering
}

func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, string, error) {
	if len(dispatch.Attachments) > albumLimit {
		return "", "", coreclient.Permanent(fmt.Errorf(
			"Telegram albums hold at most %d attachments, dispatch carries %d",
			albumLimit, len(dispatch.Attachments),
		))
	}
	peer, err := s.peers.InputPeer(ctx, dispatch.TargetChatId)
	if err != nil {
		return "", "", err
	}

	var provisionedThread string
	topicRoot := 0
	if provision := dispatch.GetProvisionThread(); provision != nil {
		topicRoot, err = s.createTopic(ctx, peer, provision.GetTitle())
		if err != nil {
			return "", "", err
		}
		provisionedThread = strconv.Itoa(topicRoot)
	} else if dispatch.TargetThreadId != nil {
		topicRoot, err = strconv.Atoi(dispatch.GetTargetThreadId())
		if err != nil {
			return "", "", coreclient.Permanent(fmt.Errorf("invalid Telegram target thread id %q: %w", dispatch.GetTargetThreadId(), err))
		}
	}

	// A system note (an info card, a change to the chat) is the bridge
	// speaking: it carries no sender header and replies to nothing, so it
	// never uses the sticker label either. Clearing these once here keeps
	// every send path below (text, media, album, text as a file) the same.
	senderName, participant := dispatch.GetSenderName(), dispatch.GetParticipant()
	nativeReplyID := replyTargetMessageID(dispatch)
	if dispatch.GetSystemNote() {
		senderName, participant, nativeReplyID = "", "", ""
	}

	builder := s.sender.To(peer).CloneBuilder()
	target := sendTarget{peer: peer}
	if nativeReplyID != "" {
		replyID, err := strconv.Atoi(nativeReplyID)
		if err != nil {
			return "", "", coreclient.Permanent(fmt.Errorf("invalid Telegram reply message ID %q: %w", nativeReplyID, err))
		}
		builder = builder.Reply(replyID)
		target.replyTo = replyID
	} else if topicRoot != 0 {
		builder = builder.Reply(topicRoot)
		target.replyTo = topicRoot
	}

	rendering := s.rendering()
	body := attributedText(senderName, participant, dispatch.GetText())
	var updates tg.UpdatesClass
	switch {
	case len(dispatch.Attachments) > 1:
		updates, err = s.sendAlbum(ctx, builder, dispatch.Attachments, body, rendering.AttachmentsAsFiles)
	case len(dispatch.Attachments) > 0:
		updates, err = s.sendAttachment(ctx, builder, target, dispatch.Attachments[0], body, senderLabel(senderName, participant), rendering.AttachmentsAsFiles)
	case textExceedsLimit(dispatch.GetText(), rendering.MaxMessageLength):
		updates, err = s.sendTextAsFile(ctx, builder, senderName, participant, dispatch.GetText())
	default:
		updates, err = builder.StyledText(ctx, body...)
	}
	if err != nil {
		return "", "", fmt.Errorf("send Telegram message: %w", err)
	}
	messageID, err := sentMessageID(updates)
	if err != nil {
		return "", "", err
	}
	return strconv.Itoa(messageID), provisionedThread, nil
}

// EditMessage applies an inbound edit to an already-delivered copy. In the
// default in-place mode it revises the original message; in repost mode it
// leaves the original untouched and posts the revised content as a new reply
// marked as an edit. Either way the original native id is returned unchanged,
// so the copy mapping continues to point at the message future edits revise.
func (s Sender) EditMessage(ctx context.Context, dispatch *wuphfpb.MessageEditDispatch) (string, error) {
	peer, err := s.peers.InputPeer(ctx, dispatch.TargetChatId)
	if err != nil {
		return "", err
	}
	messageID, err := strconv.Atoi(dispatch.TargetPlatformMessageId)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target message id %q: %w", dispatch.TargetPlatformMessageId, err))
	}
	body := attributedText(dispatch.GetSenderName(), dispatch.GetParticipant(), dispatch.GetText())
	if s.rendering().EditMode == config.EditModeRepost {
		body = append(body, styling.Italic("\n(edited)"))
		if _, err := s.sender.To(peer).CloneBuilder().Reply(messageID).StyledText(ctx, body...); err != nil {
			return "", fmt.Errorf("repost Telegram edit: %w", err)
		}
		return dispatch.TargetPlatformMessageId, nil
	}
	if _, err := s.sender.To(peer).Edit(messageID).StyledText(ctx, body...); err != nil {
		return "", fmt.Errorf("edit Telegram message: %w", err)
	}
	return dispatch.TargetPlatformMessageId, nil
}

// DeleteMessage applies an inbound delete to an already-delivered copy. In the
// default revoke mode it deletes the mirrored message; in tombstone mode it
// leaves it in place and posts a notice replying to it. The original native id
// is returned unchanged for the acknowledgement.
func (s Sender) DeleteMessage(ctx context.Context, dispatch *wuphfpb.MessageDeleteDispatch) (string, error) {
	peer, err := s.peers.InputPeer(ctx, dispatch.TargetChatId)
	if err != nil {
		return "", err
	}
	messageID, err := strconv.Atoi(dispatch.TargetPlatformMessageId)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target message id %q: %w", dispatch.TargetPlatformMessageId, err))
	}
	if s.rendering().DeleteMode == config.DeleteModeTombstone {
		if _, err := s.sender.To(peer).CloneBuilder().Reply(messageID).StyledText(ctx, styling.Italic("🗑 message deleted")); err != nil {
			return "", fmt.Errorf("post Telegram tombstone: %w", err)
		}
		return dispatch.TargetPlatformMessageId, nil
	}
	if err := s.deleteMessages(ctx, peer, messageID); err != nil {
		return "", err
	}
	return dispatch.TargetPlatformMessageId, nil
}

// ApplyReaction renders a reaction change as text, the chosen display for
// mirrored reactions on Telegram: a short reply to the reacted message naming
// the reactor and their emoji, or noting they cleared it. The reacted copy's
// native id is returned unchanged for the acknowledgement, so the reaction
// dispatch leaves the parent's copy mapping intact (the posted note is not
// tracked as a copy).
func (s Sender) ApplyReaction(ctx context.Context, dispatch *wuphfpb.ReactionDispatch) (string, error) {
	peer, err := s.peers.InputPeer(ctx, dispatch.TargetChatId)
	if err != nil {
		return "", err
	}
	messageID, err := strconv.Atoi(dispatch.TargetPlatformMessageId)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target message id %q: %w", dispatch.TargetPlatformMessageId, err))
	}
	reactor := dispatch.GetReactorName()
	if reactor == "" {
		reactor = dispatch.GetReactor()
	}
	var note []styling.StyledTextOption
	if emoji := dispatch.GetEmoji(); emoji != "" {
		note = []styling.StyledTextOption{styling.Italic(fmt.Sprintf("%s reacted %s", reactor, emoji))}
	} else {
		note = []styling.StyledTextOption{styling.Italic(fmt.Sprintf("%s removed their reaction", reactor))}
	}
	if _, err := s.sender.To(peer).CloneBuilder().Reply(messageID).StyledText(ctx, note...); err != nil {
		return "", fmt.Errorf("post Telegram reaction note: %w", err)
	}
	return dispatch.TargetPlatformMessageId, nil
}

// AckDelivery surfaces a delivery acknowledgement as a 👍 reaction on the
// message the user sent, confirming it reached the source. MarkSeen surfaces a
// read receipt as a 👀 reaction. Both leave the target's native id unchanged
// for the acknowledgement.
func (s Sender) AckDelivery(ctx context.Context, dispatch *wuphfpb.DeliveryAckDispatch) (string, error) {
	return s.react(ctx, dispatch.TargetChatId, dispatch.TargetPlatformMessageId, "👍")
}

func (s Sender) MarkSeen(ctx context.Context, dispatch *wuphfpb.SeenReceiptDispatch) (string, error) {
	return s.react(ctx, dispatch.TargetChatId, dispatch.TargetPlatformMessageId, "👀")
}

// react sets one of the bridge's own native reactions on a message. It is used
// only for the bridge's signalling emoji (delivery 👍, seen 👀); mirrored source
// reactions render as text instead.
func (s Sender) react(ctx context.Context, chatID, messageID, emoji string) (string, error) {
	peer, err := s.peers.InputPeer(ctx, chatID)
	if err != nil {
		return "", err
	}
	id, err := strconv.Atoi(messageID)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target message id %q: %w", messageID, err))
	}
	request := &tg.MessagesSendReactionRequest{Peer: peer, MsgID: id}
	request.SetReaction([]tg.ReactionClass{&tg.ReactionEmoji{Emoticon: emoji}})
	if _, err := s.api.MessagesSendReaction(ctx, request); err != nil {
		return "", fmt.Errorf("send Telegram reaction: %w", err)
	}
	return messageID, nil
}

// RetitleThread renames an already-provisioned forum topic when its source
// conversation's title changes. The thread id is returned unchanged for the
// acknowledgement, as a retitle creates no new message.
func (s Sender) RetitleThread(ctx context.Context, dispatch *wuphfpb.MessageRetitleDispatch) (string, error) {
	peer, err := s.peers.InputPeer(ctx, dispatch.TargetChatId)
	if err != nil {
		return "", err
	}
	topicID, err := strconv.Atoi(dispatch.TargetThreadId)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target thread id %q: %w", dispatch.TargetThreadId, err))
	}
	request := &tg.MessagesEditForumTopicRequest{
		Peer:    peer,
		TopicID: topicID,
	}
	request.SetTitle(dispatch.Title)
	if _, err := s.api.MessagesEditForumTopic(ctx, request); err != nil {
		return "", fmt.Errorf("edit Telegram forum topic: %w", err)
	}
	return dispatch.TargetThreadId, nil
}

// labelCallbackData marks the inline button that carries a sticker's sender
// label. The button only displays the label; pressing it does nothing.
const labelCallbackData = "wuphf:label"

// senderLabel is the attribution header a mirrored message carries ("🗣️: name"),
// or empty when the dispatch has no participant (an unlabelled direct chat). The
// name falls back to the raw participant id when the source supplied none.
func senderLabel(senderName, participant string) string {
	if participant == "" {
		return ""
	}
	if senderName == "" {
		senderName = participant
	}
	return "🗣️: " + senderName
}

// attributedText renders a dispatch body for Telegram, prefixing a bold sender
// header ("🗣️: name") on its own line for group messages so the mirror shows who
// authored each message. Attribution is added only when a participant is present
// (a group context); direct chats are left unlabelled. The name falls back to
// the raw participant id when the source supplied no display name.
func attributedText(senderName, participant, text string) []message.StyledTextOption {
	label := senderLabel(senderName, participant)
	if label == "" {
		if text == "" {
			return nil
		}
		return []message.StyledTextOption{styling.Plain(text)}
	}
	if text == "" {
		return []message.StyledTextOption{styling.Bold(label)}
	}
	return []message.StyledTextOption{styling.Bold(label), styling.Plain("\n\n" + text)}
}

// plainAttributedText renders the sender attribution and body as a single plain
// string, used as the contents of the spill-over text file when a message
// exceeds the target's length limit.
func plainAttributedText(senderName, participant, text string) string {
	label := senderLabel(senderName, participant)
	if label == "" {
		return text
	}
	if text == "" {
		return label
	}
	return label + "\n\n" + text
}

// textExceedsLimit reports whether the text is longer than the configured
// maximum, measured in UTF-16 code units to match how Telegram counts message
// length. A zero limit means unbounded.
func textExceedsLimit(text string, limit uint32) bool {
	if limit == 0 {
		return false
	}
	return len(utf16.Encode([]rune(text))) > int(limit)
}

// sendTextAsFile delivers an over-length message as a plain-text document so the
// platform's per-message length cap does not truncate or reject it.
func (s Sender) sendTextAsFile(
	ctx context.Context,
	builder *message.Builder,
	senderName, participant, text string,
) (tg.UpdatesClass, error) {
	content := plainAttributedText(senderName, participant, text)
	file, err := builder.Upload(message.FromBytes("message.txt", []byte(content))).AsInputFile(ctx)
	if err != nil {
		return nil, fmt.Errorf("upload Telegram text file: %w", err)
	}
	return builder.Media(ctx, namedFile(file, "message.txt", "text/plain; charset=utf-8"))
}

// createTopic creates a forum topic in the container and returns its root
// message id, which doubles as the topic (thread) id for later sends.
func (s Sender) createTopic(ctx context.Context, peer tg.InputPeerClass, title string) (int, error) {
	if title == "" {
		title = "conversation"
	}
	updates, err := s.api.MessagesCreateForumTopic(ctx, &tg.MessagesCreateForumTopicRequest{
		Peer:     peer,
		Title:    title,
		RandomID: rand.Int63(),
	})
	if err != nil {
		return 0, fmt.Errorf("create Telegram forum topic: %w", err)
	}
	topicRoot, err := sentMessageID(updates)
	if err != nil {
		return 0, fmt.Errorf("resolve created Telegram topic id: %w", err)
	}
	return topicRoot, nil
}

func (s Sender) sendAttachment(
	ctx context.Context,
	builder *message.Builder,
	target sendTarget,
	attachment *wuphfpb.Attachment,
	captionOptions []message.StyledTextOption,
	label string,
	asFile bool,
) (tg.UpdatesClass, error) {
	bytes, err := s.media.Download(ctx, attachment.MediaId)
	if err != nil {
		return nil, fmt.Errorf("download media from core: %w", err)
	}
	// Stickers and GIFs carry motion that is lost if they degrade to a file, so
	// they are handled before the extension/force-file logic that governs plain
	// media. An animated sticker is rendered as a Telegram WebM video sticker; a
	// GIF (an animated video) as a Telegram animation.
	if attachment.Kind == wuphfpb.Attachment_KIND_STICKER {
		return s.sendSticker(ctx, builder, target, attachment, bytes, label)
	}
	if attachment.Kind == wuphfpb.Attachment_KIND_VIDEO && attachment.GetAnimated() && !asFile {
		name := gifUploadName(attachment)
		file, err := builder.Upload(message.FromBytes(name, bytes)).AsInputFile(ctx)
		if err != nil {
			return nil, fmt.Errorf("upload Telegram animation: %w", err)
		}
		return builder.Media(ctx, message.Media(animationDocument(file, name, attachment.GetMimeType(), bytes), captionOptions...))
	}
	name, hasExtension := uploadName(attachment)
	upload := builder.Upload(message.FromBytes(name, bytes))
	// Photo/video/audio uploads are validated by Telegram against the filename
	// extension. When we cannot determine one, send the bytes as a document
	// instead, which carries no extension requirement, rather than risk a
	// PHOTO_EXT_INVALID (or equivalent) rejection. The delivery policy can also
	// force every attachment to a document regardless of its kind.
	kind := attachment.Kind
	if asFile || !hasExtension {
		kind = wuphfpb.Attachment_KIND_FILE
	}
	switch kind {
	case wuphfpb.Attachment_KIND_IMAGE:
		return upload.Photo(ctx, captionOptions...)
	case wuphfpb.Attachment_KIND_VIDEO:
		return upload.Video(ctx, captionOptions...)
	case wuphfpb.Attachment_KIND_AUDIO:
		return upload.Audio(ctx, captionOptions...)
	case wuphfpb.Attachment_KIND_FILE:
		file, err := upload.AsInputFile(ctx)
		if err != nil {
			return nil, fmt.Errorf("upload Telegram file: %w", err)
		}
		return builder.Media(ctx, namedFile(file, name, attachment.GetMimeType(), captionOptions...))
	default:
		return nil, coreclient.Permanent(fmt.Errorf("unsupported outbound attachment kind %s", attachment.Kind))
	}
}

// namedFile is a plain-file document that carries its file name and MIME type.
// Telegram does not read either from the uploaded file, so without them a file
// shows as "file.unknown" and opens with no type.
func namedFile(file tg.InputFileClass, name, mimeType string, caption ...message.StyledTextOption) *message.UploadedDocumentBuilder {
	document := message.File(file, caption...).Filename(name)
	if mimeType != "" {
		document = document.MIME(mimeType)
	}
	return document
}

// albumLimit is the number of items Telegram accepts in one grouped media
// message. A dispatch carrying more is rejected as permanently undeliverable
// rather than split, so a partly-sent album can never be duplicated by a
// redelivery.
const albumLimit = 10

// sendAlbum delivers several attachments as one grouped media message. The
// caption rides on the first item, which is how Telegram shows a single caption
// under an album, and the group is sent in exactly one API call so there is no
// partial-send state to reconcile if it fails.
//
// Telegram will not group arbitrary media: photos and videos group with each
// other, audio with audio, documents with documents, and nothing groups with a
// sticker or an animation. When the attachments do not all fall in one group
// they are sent as documents instead, which accepts any file type — the album
// survives as a unit at the cost of inline previews for some items.
func (s Sender) sendAlbum(
	ctx context.Context,
	builder *message.Builder,
	attachments []*wuphfpb.Attachment,
	captionOptions []message.StyledTextOption,
	asFile bool,
) (tg.UpdatesClass, error) {
	asFile = asFile || !groupable(attachments)
	options := make([]message.MultiMediaOption, 0, len(attachments))
	for i, attachment := range attachments {
		// Only the first item carries the caption; the rest are bare media.
		var caption []message.StyledTextOption
		if i == 0 {
			caption = captionOptions
		}
		option, err := s.albumItem(ctx, builder, attachment, caption, asFile)
		if err != nil {
			return nil, err
		}
		options = append(options, option)
	}
	return builder.Album(ctx, options[0], options[1:]...)
}

// albumItem uploads one attachment and returns it as a media option for the
// album. It mirrors sendAttachment's kind handling, minus the paths that cannot
// appear in an album (stickers and animations, which groupable rejects).
func (s Sender) albumItem(
	ctx context.Context,
	builder *message.Builder,
	attachment *wuphfpb.Attachment,
	captionOptions []message.StyledTextOption,
	asFile bool,
) (message.MultiMediaOption, error) {
	bytes, err := s.media.Download(ctx, attachment.MediaId)
	if err != nil {
		return nil, fmt.Errorf("download media from core: %w", err)
	}
	name, hasExtension := uploadName(attachment)
	file, err := builder.Upload(message.FromBytes(name, bytes)).AsInputFile(ctx)
	if err != nil {
		return nil, fmt.Errorf("upload Telegram album item: %w", err)
	}
	kind := attachment.Kind
	if asFile || !hasExtension {
		kind = wuphfpb.Attachment_KIND_FILE
	}
	switch kind {
	case wuphfpb.Attachment_KIND_IMAGE:
		return message.UploadedPhoto(file, captionOptions...), nil
	case wuphfpb.Attachment_KIND_VIDEO:
		return message.Video(file, captionOptions...), nil
	case wuphfpb.Attachment_KIND_AUDIO:
		return message.Audio(file, captionOptions...), nil
	case wuphfpb.Attachment_KIND_FILE:
		return namedFile(file, name, attachment.GetMimeType(), captionOptions...), nil
	default:
		return nil, coreclient.Permanent(fmt.Errorf("unsupported outbound attachment kind %s", attachment.Kind))
	}
}

// groupable reports whether every attachment belongs to the same Telegram album
// group, so the album can keep their native kinds. Stickers and animations are
// never groupable, and an attachment with no usable extension degrades to a
// document anyway, so it only groups with other documents.
func groupable(attachments []*wuphfpb.Attachment) bool {
	first := ""
	for _, attachment := range attachments {
		group := albumGroup(attachment)
		if group == "" {
			return false
		}
		if first == "" {
			first = group
		} else if group != first {
			return false
		}
	}
	return true
}

// albumGroup names the album group an attachment belongs to, or "" when it
// cannot be grouped at all.
func albumGroup(attachment *wuphfpb.Attachment) string {
	if attachment.Kind == wuphfpb.Attachment_KIND_STICKER {
		return ""
	}
	if attachment.Kind == wuphfpb.Attachment_KIND_VIDEO && attachment.GetAnimated() {
		return ""
	}
	if _, hasExtension := uploadName(attachment); !hasExtension {
		return "file"
	}
	switch attachment.Kind {
	case wuphfpb.Attachment_KIND_IMAGE, wuphfpb.Attachment_KIND_VIDEO:
		return "visual"
	case wuphfpb.Attachment_KIND_AUDIO:
		return "audio"
	case wuphfpb.Attachment_KIND_FILE:
		return "file"
	default:
		return ""
	}
}

// sendSticker delivers a sticker, preserving animation. A static sticker is sent
// as-is (WebP). An animated sticker is delivered as a WebM/VP9 video sticker:
// WhatsApp animated stickers arrive as animated WebP, which Telegram renders only
// as a still frame, so they are transcoded to WebM first. The transcode runs on
// the core's media.convert provider; when no provider is available or the
// conversion fails, the sticker degrades to a static sticker (the first frame)
// rather than dropping the message.
//
// Telegram stickers take no caption, so the sender label a captioned message
// carries in its text (see senderLabel) is attached another way: a bot session
// shows it as an inline button under the sticker. A user session cannot attach
// buttons itself, so it sends the sticker as the helper bot's inline result,
// which carries the same button (InlineStickers). Without a helper, or when the
// helper fails or takes longer than inlineStickerBudget, it posts the label as
// a short message ("(Sticker)" on a second line) and sends the sticker as a
// reply to it.
func (s Sender) sendSticker(
	ctx context.Context,
	builder *message.Builder,
	target sendTarget,
	attachment *wuphfpb.Attachment,
	data []byte,
	label string,
) (tg.UpdatesClass, error) {
	// Convert an animated sticker once, whichever route sends it.
	payload, mime, filename := data, "image/webp", "sticker.webp"
	if attachment.GetAnimated() {
		if webm, err := s.animatedStickerWebM(ctx, attachment, data); err == nil {
			payload, mime, filename = webm, "video/webm", "sticker.webm"
		}
		// Otherwise deliver the original as a static sticker (first frame).
	}
	if label != "" {
		if s.bot {
			builder = builder.Markup(markup.InlineRow(markup.Callback(label, []byte(labelCallbackData))))
		} else {
			if s.stickers.Ready() {
				inlineCtx, cancel := context.WithTimeout(ctx, s.stickers.budget)
				updates, err := s.stickers.Send(inlineCtx, target.peer, target.replyTo, payload, mime, filename, label)
				cancel()
				if err == nil {
					return updates, nil
				}
			}
			// The label message stands in for the caption: it names the sender
			// and what follows, and the sticker replies to it so the two read as
			// one message.
			updates, err := builder.StyledText(ctx, styling.Bold(label), styling.Italic("\n(Sticker)"))
			if err != nil {
				return nil, fmt.Errorf("send Telegram sticker label: %w", err)
			}
			labelID, err := sentMessageID(updates)
			if err != nil {
				return nil, fmt.Errorf("send Telegram sticker label: %w", err)
			}
			builder = builder.Reply(labelID)
		}
	}
	if mime == "video/webm" {
		if file, err := builder.Upload(message.FromBytes(filename, payload)).AsInputFile(ctx); err == nil {
			media := message.UploadedDocument(file).MIME(mime).Filename(filename).UploadedSticker()
			return builder.Media(ctx, media)
		}
		// Fall back to the original as a static sticker (first frame).
		payload = data
	}
	return builder.Upload(message.FromBytes("sticker.webp", payload)).Sticker(ctx)
}

// sendTarget is where a message goes: the peer, and the message it replies to
// (0 for none), which a send that bypasses the builder needs.
type sendTarget struct {
	peer    tg.InputPeerClass
	replyTo int
}

// animatedStickerWebM returns the sticker as WebM/VP9 bytes, transcoding from
// animated WebP through media.convert when needed. A source already in WebM (an
// animated sticker that originated elsewhere as video) is passed through
// unchanged.
func (s Sender) animatedStickerWebM(ctx context.Context, attachment *wuphfpb.Attachment, data []byte) ([]byte, error) {
	if attachment.GetMimeType() == "video/webm" {
		return data, nil
	}
	return s.converter.Convert(ctx, attachment.GetMediaId(), attachment.GetMimeType(), attachment.GetFileName(), "video/webm")
}

// gifUploadName names a GIF upload: the source's name when it has an
// extension, else animation.gif for a real GIF and animation.mp4 (the container
// Telegram animations use) for the rest.
func gifUploadName(attachment *wuphfpb.Attachment) string {
	if name := attachment.GetFileName(); name != "" && path.Ext(name) != "" {
		return name
	}
	if attachment.GetMimeType() == "image/gif" {
		return "animation.gif"
	}
	return "animation.mp4"
}

// animationDocument is a GIF as Telegram's own apps send one: video/mp4 (or a
// real image/gif) with its filename, the video attribute (size and length, read
// from the MP4 header) and the animated attribute, marked silent. gotd's GIF
// builder sends only image/gif and the animated attribute, which Telegram
// Desktop plays but the iPhone app shows as a nameless "file".
func animationDocument(file tg.InputFileClass, name, mime string, data []byte) *tg.InputMediaUploadedDocument {
	if mime != "image/gif" {
		mime = "video/mp4"
	}
	attributes := []tg.DocumentAttributeClass{&tg.DocumentAttributeFilename{FileName: name}}
	if mime == "video/mp4" {
		video := &tg.DocumentAttributeVideo{SupportsStreaming: true, Nosound: true}
		if info, ok := probeMP4(data); ok {
			video.W, video.H, video.Duration = info.width, info.height, info.seconds
		}
		attributes = append(attributes, video)
	}
	return &tg.InputMediaUploadedDocument{
		File:         file,
		MimeType:     mime,
		NosoundVideo: true,
		Attributes:   append(attributes, &tg.DocumentAttributeAnimated{}),
	}
}

// uploadName builds the filename for a Telegram media upload and reports whether
// it carries an extension. Telegram infers a file's type from its extension and
// rejects an extensionless typed upload (e.g. PHOTO_EXT_INVALID), yet WhatsApp
// images arrive with no filename. An existing extension is kept; otherwise one
// is derived from the MIME type. When neither yields an extension the caller
// falls back to a document upload, so no extension is invented here.
func uploadName(attachment *wuphfpb.Attachment) (string, bool) {
	name := attachment.GetFileName()
	if name != "" && path.Ext(name) != "" {
		return name, true
	}
	if name == "" {
		name = "attachment"
	}
	if ext := mimeExtension(attachment.GetMimeType()); ext != "" {
		return name + ext, true
	}
	return name, false
}

func mimeExtension(mimeType string) string {
	mediaType, _, err := mime.ParseMediaType(mimeType)
	if err != nil {
		return ""
	}
	return mimeExtensions[mediaType]
}

// mimeExtensions maps the media types WhatsApp commonly reports to an extension
// Telegram accepts. mime.ExtensionsByType is avoided as it is locale/OS
// dependent and can return forms (e.g. .jfif for image/jpeg) Telegram rejects.
var mimeExtensions = map[string]string{
	"image/jpeg": ".jpg",
	"image/png":  ".png",
	"image/webp": ".webp",
	"image/gif":  ".gif",
	"video/mp4":  ".mp4",
	"audio/ogg":  ".ogg",
	"audio/mpeg": ".mp3",
	"audio/mp4":  ".m4a",
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

// deleteMessages revokes messages in peer for everyone.
func (s Sender) deleteMessages(ctx context.Context, peer tg.InputPeerClass, ids ...int) error {
	var err error
	switch input := peer.(type) {
	case *tg.InputPeerChannel:
		_, err = s.api.ChannelsDeleteMessages(ctx, &tg.ChannelsDeleteMessagesRequest{
			Channel: &tg.InputChannel{ChannelID: input.ChannelID, AccessHash: input.AccessHash},
			ID:      ids,
		})
	default:
		_, err = s.api.MessagesDeleteMessages(ctx, &tg.MessagesDeleteMessagesRequest{
			Revoke: true,
			ID:     ids,
		})
	}
	if err != nil {
		return fmt.Errorf("delete Telegram message: %w", err)
	}
	return nil
}
