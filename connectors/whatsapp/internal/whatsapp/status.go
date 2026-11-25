package whatsappconnector

import (
	"context"
	"fmt"

	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"
)

// isStatusBroadcast reports whether chat is WhatsApp's story feed. A send to
// it is posted as a story to every contact the status privacy allows, so the
// sender never does that for a dispatch (plan 083): the core keeps the chat
// receive-only, and this is the backstop if it ever does not.
func isStatusBroadcast(chat types.JID) bool {
	return chat.ToNonAD() == types.StatusBroadcastJID
}

// refuseStatusPost is the permanent failure for a dispatch that would post
// to status@broadcast.
func refuseStatusPost(operation string) error {
	return coreclient.Permanent(fmt.Errorf("refusing to %s status@broadcast: that would post a story to your contacts", operation))
}

// reactToStory sends the owner's reaction on someone's story the way
// WhatsApp's own quick reactions do: as an emoji message in the poster's DM,
// quoting the story. A send to status@broadcast would reach everyone who sees
// the owner's stories, and whatsmeow cannot address only the poster. A
// cleared reaction, or one on the owner's own story, sends nothing.
func (s Sender) reactToStory(ctx context.Context, dispatch *wuphfpb.ReactionDispatch, statusMode types.AddressingMode) (string, error) {
	storyID := dispatch.GetTargetPlatformMessageId()
	poster := dispatch.GetTargetParticipant()
	if poster == "" {
		poster = dispatch.GetTargetSender()
	}
	if dispatch.GetEmoji() == "" || dispatch.GetTargetAuthoredByBridge() || poster == "" {
		return storyID, nil
	}
	posterJID, err := types.ParseJID(poster)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("parse story poster %q: %w", poster, err))
	}
	if isStatusBroadcast(posterJID) {
		return storyID, nil
	}
	dm, _, err := s.target(ctx, posterJID.ToNonAD().String())
	if err != nil {
		return "", err
	}
	quoted := s.addressing.InMode(ctx, posterJID.ToNonAD(), statusMode).String()
	message := &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
		Text:        proto.String(dispatch.GetEmoji()),
		ContextInfo: storyQuote(storyID, quoted),
	}}
	if _, err := s.client.SendMessage(ctx, dm, message); err != nil {
		return "", fmt.Errorf("react to WhatsApp story: %w", err)
	}
	return storyID, nil
}

// storyQuote is the quote of a story in its poster's DM: the story's id, its
// poster, and status@broadcast as the chat it lives in.
func storyQuote(storyID, poster string) *waE2E.ContextInfo {
	return &waE2E.ContextInfo{
		StanzaID:      proto.String(storyID),
		Participant:   proto.String(poster),
		RemoteJID:     proto.String(types.StatusBroadcastJID.String()),
		QuotedMessage: &waE2E.Message{Conversation: proto.String("")},
	}
}

// Text statuses use WhatsApp's default look (plan 084): a dark teal
// background, white text, the system font.
const (
	statusBackground uint32 = 0xFF075E54
	statusText       uint32 = 0xFFFFFFFF
)

// postStatus posts the dispatch as the owner's story (plan 084): a text
// status, or each attachment as one (with the text as the first one's
// caption). It quotes nothing and carries no disappearing timer: a story
// keeps its own 24 hours. WhatsApp sends it to everyone the status privacy
// allows; that is the point of an explicit post.
func (s Sender) postStatus(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch, chat types.JID) (string, error) {
	if len(dispatch.Attachments) == 0 {
		if dispatch.GetText() == "" {
			return "", coreclient.Permanent(fmt.Errorf("an empty story cannot be posted"))
		}
		response, err := s.client.SendMessage(ctx, chat, textStatus(dispatch.GetText()))
		if err != nil {
			return "", fmt.Errorf("post WhatsApp text status: %w", err)
		}
		return string(response.ID), nil
	}
	var first string
	for index, attachment := range dispatch.Attachments {
		caption := ""
		if index == 0 {
			caption = dispatch.GetText()
		}
		message, err := s.attachmentMessage(ctx, attachment, caption, false)
		if err != nil {
			return "", attachmentGroupError(err, index)
		}
		response, err := s.client.SendMessage(ctx, chat, message)
		if err != nil {
			return "", attachmentGroupError(fmt.Errorf("post WhatsApp media status: %w", err), index)
		}
		if index == 0 {
			first = string(response.ID)
		}
	}
	return first, nil
}

// textStatus is a text story in the default look.
func textStatus(text string) *waE2E.Message {
	return &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
		Text:           proto.String(text),
		BackgroundArgb: proto.Uint32(statusBackground),
		TextArgb:       proto.Uint32(statusText),
		Font:           waE2E.ExtendedTextMessage_SYSTEM.Enum(),
	}}
}

// statusAudience says who sees the owner's stories: "My contacts", "My
// contacts except 3", "Only 5 contacts". It reads the first privacy option,
// the one whatsmeow sends a story to (getStatusBroadcastRecipients).
func statusAudience(privacy []types.StatusPrivacy) string {
	if len(privacy) == 0 {
		return ""
	}
	active := privacy[0]
	switch active.Type {
	case types.StatusPrivacyTypeBlacklist:
		return fmt.Sprintf("My contacts except %d", len(active.List))
	case types.StatusPrivacyTypeWhitelist:
		if len(active.List) == 1 {
			return "Only 1 contact"
		}
		return fmt.Sprintf("Only %d contacts", len(active.List))
	default:
		return "My contacts"
	}
}
