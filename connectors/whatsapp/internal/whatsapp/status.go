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
