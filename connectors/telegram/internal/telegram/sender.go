package telegramconnector

import (
	"context"
	"errors"
	"fmt"
	"math/rand"
	"mime"
	"path"
	"strconv"

	coreclient "github.com/akshettrj/wuphf/connectors/telegram/internal/core"
	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/telegram/message/styling"
	"github.com/gotd/td/tg"
)

type Sender struct {
	api    *tg.Client
	sender *message.Sender
	peers  *PeerCache
	media  MediaBroker
}

func NewSender(api *tg.Client, peers *PeerCache, media MediaBroker) Sender {
	return Sender{
		api:    api,
		sender: message.NewSender(api),
		peers:  peers,
		media:  media,
	}
}

func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, string, error) {
	if len(dispatch.Attachments) > 1 {
		return "", "", coreclient.Permanent(errors.New("multiple attachment dispatch is not implemented"))
	}
	peer, err := s.peers.InputPeer(dispatch.TargetChatId)
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

	builder := s.sender.To(peer).CloneBuilder()
	if nativeReplyID := replyTargetMessageID(dispatch); nativeReplyID != "" {
		replyID, err := strconv.Atoi(nativeReplyID)
		if err != nil {
			return "", "", coreclient.Permanent(fmt.Errorf("invalid Telegram reply message ID %q: %w", nativeReplyID, err))
		}
		builder = builder.Reply(replyID)
	} else if topicRoot != 0 {
		builder = builder.Reply(topicRoot)
	}

	body := attributedText(dispatch.GetSenderName(), dispatch.GetParticipant(), dispatch.GetText())
	var updates tg.UpdatesClass
	if len(dispatch.Attachments) == 0 {
		updates, err = builder.StyledText(ctx, body...)
	} else {
		updates, err = s.sendAttachment(ctx, builder, dispatch.Attachments[0], body)
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
	peer, err := s.peers.InputPeer(dispatch.TargetChatId)
	if err != nil {
		return "", err
	}
	messageID, err := strconv.Atoi(dispatch.TargetPlatformMessageId)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target message id %q: %w", dispatch.TargetPlatformMessageId, err))
	}
	body := attributedText(dispatch.GetSenderName(), dispatch.GetParticipant(), dispatch.GetText())
	if dispatch.GetEditMode() == wuphfpb.EditMode_EDIT_MODE_REPOST {
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
	peer, err := s.peers.InputPeer(dispatch.TargetChatId)
	if err != nil {
		return "", err
	}
	messageID, err := strconv.Atoi(dispatch.TargetPlatformMessageId)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target message id %q: %w", dispatch.TargetPlatformMessageId, err))
	}
	if dispatch.GetDeleteMode() == wuphfpb.DeleteMode_DELETE_MODE_TOMBSTONE {
		if _, err := s.sender.To(peer).CloneBuilder().Reply(messageID).StyledText(ctx, styling.Italic("🗑 message deleted")); err != nil {
			return "", fmt.Errorf("post Telegram tombstone: %w", err)
		}
		return dispatch.TargetPlatformMessageId, nil
	}
	switch input := peer.(type) {
	case *tg.InputPeerChannel:
		_, err = s.api.ChannelsDeleteMessages(ctx, &tg.ChannelsDeleteMessagesRequest{
			Channel: &tg.InputChannel{ChannelID: input.ChannelID, AccessHash: input.AccessHash},
			ID:      []int{messageID},
		})
	default:
		_, err = s.api.MessagesDeleteMessages(ctx, &tg.MessagesDeleteMessagesRequest{
			Revoke: true,
			ID:     []int{messageID},
		})
	}
	if err != nil {
		return "", fmt.Errorf("delete Telegram message: %w", err)
	}
	return dispatch.TargetPlatformMessageId, nil
}

// RetitleThread renames an already-provisioned forum topic when its source
// conversation's title changes. The thread id is returned unchanged for the
// acknowledgement, as a retitle creates no new message.
func (s Sender) RetitleThread(ctx context.Context, dispatch *wuphfpb.MessageRetitleDispatch) (string, error) {
	peer, err := s.peers.InputPeer(dispatch.TargetChatId)
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

// attributedText renders a dispatch body for Telegram, prefixing a bold sender
// header ("🗣️: name") on its own line for group messages so the mirror shows who
// authored each message. Attribution is added only when a participant is present
// (a group context); direct chats are left unlabelled. The name falls back to
// the raw participant id when the source supplied no display name.
func attributedText(senderName, participant, text string) []message.StyledTextOption {
	if participant == "" {
		if text == "" {
			return nil
		}
		return []message.StyledTextOption{styling.Plain(text)}
	}
	label := senderName
	if label == "" {
		label = participant
	}
	label = "🗣️: " + label
	if text == "" {
		return []message.StyledTextOption{styling.Bold(label)}
	}
	return []message.StyledTextOption{styling.Bold(label), styling.Plain("\n\n" + text)}
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
	attachment *wuphfpb.Attachment,
	captionOptions []message.StyledTextOption,
) (tg.UpdatesClass, error) {
	bytes, err := s.media.Download(ctx, attachment.MediaId)
	if err != nil {
		return nil, fmt.Errorf("download media from core: %w", err)
	}
	name, hasExtension := uploadName(attachment)
	upload := builder.Upload(message.FromBytes(name, bytes))
	// Photo/video/audio/sticker uploads are validated by Telegram against the
	// filename extension. When we cannot determine one, send the bytes as a
	// document instead, which carries no extension requirement, rather than
	// risk a PHOTO_EXT_INVALID (or equivalent) rejection.
	kind := attachment.Kind
	if !hasExtension {
		kind = wuphfpb.Attachment_KIND_FILE
	}
	switch kind {
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
		return nil, coreclient.Permanent(fmt.Errorf("unsupported outbound attachment kind %s", attachment.Kind))
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
