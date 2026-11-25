package whatsappconnector

import (
	"context"
	"errors"
	"testing"

	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

const channel = "120363000000000001@newsletter"

// Only an explicit post goes to a channel (plan 085): a post reaches every
// follower.
func TestAPlainSendToAChannelIsRefused(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	text := "hi"
	_, _, err := statusSender(client).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{TargetChatId: channel, Text: &text})
	if !errors.As(err, new(coreclient.PermanentError)) || client.sent != nil {
		t.Fatalf("SendMessage() error = %v; sent %v", err, client.sent)
	}
}

// A text post goes to the channel as text.
func TestATextPostGoesToTheChannel(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	text := "news"
	id, _, err := statusSender(client).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{TargetChatId: channel, Text: &text, Post: true})
	if err != nil || id != "sent-id" || client.sentTo.String() != channel || client.sent.GetConversation() != "news" {
		t.Fatalf("SendMessage() = %q, %v; sent %v to %s", id, err, client.sent, client.sentTo)
	}
}

// A photo post is uploaded unencrypted, and its handle goes with the send.
func TestAPhotoPostIsUploadedForTheChannel(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	text := "look"
	mime := "image/jpeg"
	_, _, err := statusSender(client).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: channel, Text: &text, Post: true,
		Attachments: []*wuphfpb.Attachment{{MediaId: "m-1", Kind: wuphfpb.Attachment_KIND_IMAGE, MimeType: &mime}},
	})
	if err != nil {
		t.Fatal(err)
	}
	if !client.newsletterUpload || client.sent.GetImageMessage().GetCaption() != "look" {
		t.Fatalf("uploaded for a channel: %v; sent %v", client.newsletterUpload, client.sent)
	}
	if len(client.extra) != 1 || client.extra[0].MediaHandle != "handle-1" {
		t.Fatalf("send extra = %+v", client.extra)
	}
}

// A reaction to a channel post sends nothing.
func TestAReactionToAChannelPostSendsNothing(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	id, err := statusSender(client).ApplyReaction(context.Background(), &wuphfpb.ReactionDispatch{
		TargetChatId: channel, TargetPlatformMessageId: "p-1", TargetSender: channel, ByOwner: true, Emoji: "❤️",
	})
	if err != nil || id != "p-1" || client.sent != nil || client.reaction != nil {
		t.Fatalf("ApplyReaction() = %q, %v; sent %v", id, err, client.sent)
	}
}

// A media upload outside a channel post stays encrypted.
func TestAnOrdinaryUploadIsNotAChannels(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	mime := "image/jpeg"
	_, _, err := statusSender(client).SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "15550001111@s.whatsapp.net",
		Attachments:  []*wuphfpb.Attachment{{MediaId: "m-1", Kind: wuphfpb.Attachment_KIND_IMAGE, MimeType: &mime}},
	})
	if err != nil || client.newsletterUpload {
		t.Fatalf("SendMessage() error = %v; channel upload %v", err, client.newsletterUpload)
	}
}
