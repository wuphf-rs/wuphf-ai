package whatsappconnector

import (
	"context"
	"errors"
	"testing"

	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"
)

func statusSender(client *stubSendClient) Sender {
	own := types.NewJID("19998887777", types.DefaultUserServer)
	return NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{ownPN: own}, liveRendering(config.RenderingOptions{}))
}

// A send or an edit to status@broadcast would post a story to every contact:
// the sender refuses it permanently and sends nothing (plan 083).
func TestSendAndEditToStatusBroadcastAreRefused(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := statusSender(client)
	text := "lol"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{TargetChatId: "status@broadcast", Text: &text})
	if err == nil || !errors.As(err, new(coreclient.PermanentError)) {
		t.Fatalf("SendMessage() error = %v, want a permanent refusal", err)
	}
	_, err = sender.EditMessage(context.Background(), &wuphfpb.MessageEditDispatch{TargetChatId: "status@broadcast", TargetPlatformMessageId: "s-1", Text: &text})
	if err == nil || !errors.As(err, new(coreclient.PermanentError)) {
		t.Fatalf("EditMessage() error = %v, want a permanent refusal", err)
	}
	if client.sent != nil {
		t.Fatalf("sent %v to %s", client.sent, client.sentTo)
	}
}

// Revoking a story (the owner's own) still goes out.
func TestDeleteOnStatusBroadcastRevokes(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	if _, err := statusSender(client).DeleteMessage(context.Background(), &wuphfpb.MessageDeleteDispatch{TargetChatId: "status@broadcast", TargetPlatformMessageId: "s-1"}); err != nil {
		t.Fatal(err)
	}
	if client.revokedKey != "s-1" || client.sentTo != types.StatusBroadcastJID {
		t.Fatalf("revoked %q to %s", client.revokedKey, client.sentTo)
	}
}

// The owner's reaction on Bob's story goes to Bob's DM as an emoji quoting
// the story, never to status@broadcast.
func TestReactionOnAStoryIsAnEmojiInThePostersDM(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	poster := "15550001111@s.whatsapp.net"
	id, err := statusSender(client).ApplyReaction(context.Background(), &wuphfpb.ReactionDispatch{
		TargetChatId: "status@broadcast", TargetPlatformMessageId: "story-1",
		TargetSender: poster, TargetParticipant: &poster, ByOwner: true, Emoji: "❤️",
	})
	if err != nil || id != "story-1" {
		t.Fatalf("ApplyReaction() = %q, %v", id, err)
	}
	if client.reaction != nil {
		t.Fatalf("built a native reaction %+v", client.reaction)
	}
	if client.sentTo.String() != poster {
		t.Fatalf("sent to %s, want %s", client.sentTo, poster)
	}
	text := client.sent.GetExtendedTextMessage()
	quote := text.GetContextInfo()
	if text.GetText() != "❤️" || quote.GetStanzaID() != "story-1" || quote.GetParticipant() != poster || quote.GetRemoteJID() != "status@broadcast" {
		t.Fatalf("sent %v", client.sent)
	}
}

// Clearing a story reaction, or reacting to the owner's own story, sends
// nothing and is acknowledged.
func TestStoryReactionsThatSendNothing(t *testing.T) {
	t.Parallel()
	poster := "15550001111@s.whatsapp.net"
	for name, dispatch := range map[string]*wuphfpb.ReactionDispatch{
		"cleared": {TargetSender: poster, TargetParticipant: &poster},
		"own":     {TargetSender: "me", TargetAuthoredByBridge: true, Emoji: "❤️"},
	} {
		t.Run(name, func(t *testing.T) {
			client := &stubSendClient{}
			dispatch.TargetChatId, dispatch.TargetPlatformMessageId, dispatch.ByOwner = "status@broadcast", "story-1", true
			id, err := statusSender(client).ApplyReaction(context.Background(), dispatch)
			if err != nil || id != "story-1" || client.sent != nil || client.reaction != nil {
				t.Fatalf("ApplyReaction() = %q, %v; sent %v", id, err, client.sent)
			}
		})
	}
}

// A reply quoting Bob's story in Bob's DM names status@broadcast as the
// quoted chat (plan 083), so WhatsApp shows a status reply.
func TestAQuoteOfAnotherChatNamesIt(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	poster := "15550001111@s.whatsapp.net"
	text := "nice"
	_, _, err := statusSender(client).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: poster, Text: &text,
		ReplyContext: &wuphfpb.ReplyContext{
			PlatformMessageId: "story-1", Sender: poster, Participant: &poster,
			ChatId: proto.String("status@broadcast"),
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	quote := client.sent.GetExtendedTextMessage().GetContextInfo()
	if client.sentTo.String() != poster || quote.GetRemoteJID() != "status@broadcast" || quote.GetStanzaID() != "story-1" || quote.GetParticipant() != poster {
		t.Fatalf("sent %v to %s", client.sent, client.sentTo)
	}
}

// An explicit post (plan 084) goes to status@broadcast as a text status in
// the default look, with no quote and no timer.
func TestAPostIsATextStatus(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	text := "hello world"
	id, _, err := statusSender(client).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "status@broadcast", Text: &text, Post: true,
	})
	if err != nil || id != "sent-id" {
		t.Fatalf("SendMessage() = %q, %v", id, err)
	}
	story := client.sent.GetExtendedTextMessage()
	if client.sentTo != types.StatusBroadcastJID || story.GetText() != text || story.GetBackgroundArgb() == 0 || story.GetContextInfo() != nil {
		t.Fatalf("posted %v to %s", client.sent, client.sentTo)
	}
}

// A photo post is an image status with the text as its caption.
func TestAPhotoPostIsAnImageStatus(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	text := "look"
	mime := "image/jpeg"
	_, _, err := statusSender(client).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "status@broadcast", Text: &text, Post: true,
		Attachments: []*wuphfpb.Attachment{{MediaId: "m-1", Kind: wuphfpb.Attachment_KIND_IMAGE, MimeType: &mime}},
	})
	if err != nil {
		t.Fatal(err)
	}
	if image := client.sent.GetImageMessage(); client.sentTo != types.StatusBroadcastJID || image.GetCaption() != "look" {
		t.Fatalf("posted %v to %s", client.sent, client.sentTo)
	}
}

// An empty post can never be posted.
func TestAnEmptyPostIsRefused(t *testing.T) {
	t.Parallel()
	_, _, err := statusSender(&stubSendClient{}).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{TargetChatId: "status@broadcast", Post: true})
	if !errors.As(err, new(coreclient.PermanentError)) {
		t.Fatalf("error = %v", err)
	}
}
