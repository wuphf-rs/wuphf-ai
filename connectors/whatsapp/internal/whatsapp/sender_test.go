package whatsappconnector

import (
	"context"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
)

type stubSendClient struct {
	sent       *waE2E.Message
	uploaded   []byte
	media      whatsmeow.MediaType
	editedKey  types.MessageID
	revokedKey types.MessageID
}

func (s *stubSendClient) SendMessage(_ context.Context, _ types.JID, message *waE2E.Message, _ ...whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
	s.sent = message
	return whatsmeow.SendResponse{ID: "sent-id"}, nil
}

func (s *stubSendClient) BuildEdit(_ types.JID, id types.MessageID, newContent *waE2E.Message) *waE2E.Message {
	s.editedKey = id
	return newContent
}

func (s *stubSendClient) BuildRevoke(_ types.JID, _ types.JID, id types.MessageID) *waE2E.Message {
	s.revokedKey = id
	return &waE2E.Message{}
}

func (s *stubSendClient) Upload(_ context.Context, bytes []byte, media whatsmeow.MediaType) (whatsmeow.UploadResponse, error) {
	s.uploaded, s.media = bytes, media
	return whatsmeow.UploadResponse{URL: "url", DirectPath: "path", FileLength: uint64(len(bytes))}, nil
}

type stubBroker struct {
	bytes []byte
}

func (*stubBroker) Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error) {
	return nil, nil
}

func (s *stubBroker) Download(context.Context, string) ([]byte, error) {
	return s.bytes, nil
}

func TestSendFileDispatch(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, types.JID{})
	text := "caption"
	fileName := "receipt.pdf"
	mime := "application/pdf"
	messageID, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
		Attachments: []*wuphfpb.Attachment{{
			MediaId:  "media-id",
			Kind:     wuphfpb.Attachment_KIND_FILE,
			FileName: &fileName,
			MimeType: &mime,
		}},
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if messageID != "sent-id" || client.media != whatsmeow.MediaDocument || string(client.uploaded) != "data" {
		t.Fatalf("SendMessage() result = %q, %q, %q", messageID, client.media, client.uploaded)
	}
	document := client.sent.GetDocumentMessage()
	if document.GetCaption() != "caption" || document.GetFileName() != "receipt.pdf" {
		t.Fatalf("sent document = %#v", document)
	}
}

func TestSendOutboundReplyBuildsQuotedContext(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	ownJID := types.NewJID("15550001111", types.DefaultUserServer)
	sender := NewSender(client, &stubBroker{}, ownJID)
	text := "child reply"
	parentText := "parent body"
	parentID := "parent-stanza-id"
	messageID, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
		ReplyContext: &wuphfpb.ReplyContext{
			PlatformMessageId: parentID,
			Text:              &parentText,
		},
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if messageID != "sent-id" {
		t.Fatalf("SendMessage() id = %q", messageID)
	}
	extended := client.sent.GetExtendedTextMessage()
	if extended.GetText() != "child reply" {
		t.Fatalf("reply text = %q", extended.GetText())
	}
	contextInfo := extended.GetContextInfo()
	if contextInfo.GetStanzaID() != parentID {
		t.Fatalf("quoted stanza ID = %q", contextInfo.GetStanzaID())
	}
	if contextInfo.GetParticipant() != ownJID.ToNonAD().String() {
		t.Fatalf("quoted participant = %q", contextInfo.GetParticipant())
	}
	if contextInfo.GetQuotedMessage().GetConversation() != parentText {
		t.Fatalf("quoted body = %q", contextInfo.GetQuotedMessage().GetConversation())
	}
}

func TestSendOutboundReplyQuotesMediaParent(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	ownJID := types.NewJID("15550001111", types.DefaultUserServer)
	sender := NewSender(client, &stubBroker{}, ownJID)
	text := "look at this"
	parentCaption := "parent photo"
	mime := "image/jpeg"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
		ReplyContext: &wuphfpb.ReplyContext{
			PlatformMessageId: "parent-stanza-id",
			Text:              &parentCaption,
			Attachments: []*wuphfpb.Attachment{{
				MediaId:  "media-id",
				Kind:     wuphfpb.Attachment_KIND_IMAGE,
				MimeType: &mime,
			}},
		},
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	quoted := client.sent.GetExtendedTextMessage().GetContextInfo().GetQuotedMessage()
	image := quoted.GetImageMessage()
	if image == nil {
		t.Fatalf("quoted message = %#v, want image", quoted)
	}
	if image.GetCaption() != parentCaption || image.GetMimetype() != mime {
		t.Fatalf("quoted image = %#v", image)
	}
}

func TestEditMessageRevisesTargetInPlace(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, types.JID{})
	text := "revised text"
	messageID, err := sender.EditMessage(context.Background(), &wuphfpb.MessageEditDispatch{
		TargetChatId:            "123@s.whatsapp.net",
		TargetPlatformMessageId: "orig-id",
		Text:                    &text,
	})
	if err != nil {
		t.Fatalf("EditMessage() error = %v", err)
	}
	if messageID != "orig-id" {
		t.Fatalf("EditMessage() id = %q, want orig-id", messageID)
	}
	if client.editedKey != "orig-id" {
		t.Fatalf("BuildEdit key = %q, want orig-id", client.editedKey)
	}
	if client.sent.GetConversation() != "revised text" {
		t.Fatalf("edited body = %q", client.sent.GetConversation())
	}
}

func TestSendRejectsLegacyReplyWithoutContext(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, types.JID{})
	reply := "parent-id"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId:             "123@s.whatsapp.net",
		ReplyToPlatformMessageId: &reply,
	})
	if err == nil || client.sent != nil {
		t.Fatalf("SendMessage() error = %v, sent = %#v", err, client.sent)
	}
}
