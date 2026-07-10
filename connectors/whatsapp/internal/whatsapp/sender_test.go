package whatsappconnector

import (
	"context"
	"testing"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
)

type stubSendClient struct {
	sent       *waE2E.Message
	sentTo     types.JID
	uploaded   []byte
	media      whatsmeow.MediaType
	editedKey  types.MessageID
	revokedKey types.MessageID
}

func (s *stubSendClient) SendMessage(_ context.Context, to types.JID, message *waE2E.Message, _ ...whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
	s.sent = message
	s.sentTo = to
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
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, types.JID{}, config.StickerMetadata{}, config.RenderingOptions{})
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

func TestSendAttachmentsAsFilesForcesDocument(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, types.JID{}, config.StickerMetadata{}, config.RenderingOptions{AttachmentsAsFiles: true})
	mime := "image/jpeg"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Attachments: []*wuphfpb.Attachment{{
			MediaId:  "media-id",
			Kind:     wuphfpb.Attachment_KIND_IMAGE,
			MimeType: &mime,
		}},
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if client.media != whatsmeow.MediaDocument {
		t.Fatalf("upload media = %q, want document", client.media)
	}
	if client.sent.GetDocumentMessage() == nil || client.sent.GetImageMessage() != nil {
		t.Fatalf("sent message = %#v, want document not image", client.sent)
	}
}

func TestSendOverLengthTextAsFile(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, types.JID{}, config.StickerMetadata{}, config.RenderingOptions{MaxMessageLength: 5})
	text := "hello world"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if client.media != whatsmeow.MediaDocument || string(client.uploaded) != text {
		t.Fatalf("upload = (%q, %q), want document carrying the text", client.media, client.uploaded)
	}
	document := client.sent.GetDocumentMessage()
	if document.GetFileName() != "message.txt" || document.GetMimetype() != "text/plain" {
		t.Fatalf("sent document = %#v, want message.txt/text/plain", document)
	}
}

func TestSendWithinLengthLimitStaysInline(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, types.JID{}, config.StickerMetadata{}, config.RenderingOptions{MaxMessageLength: 5})
	text := "hi"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if client.sent.GetConversation() != text || client.sent.GetDocumentMessage() != nil {
		t.Fatalf("sent message = %#v, want inline conversation", client.sent)
	}
}

func TestTextExceedsLimit(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		text  string
		limit uint32
		want  bool
	}{
		{"zero limit is unbounded", "anything goes", 0, false},
		{"within limit", "hi", 5, false},
		{"at limit", "hello", 5, false},
		{"over limit", "hello!", 5, true},
		{"counts runes not bytes", "héllo", 5, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := textExceedsLimit(tc.text, tc.limit); got != tc.want {
				t.Fatalf("textExceedsLimit(%q, %d) = %t, want %t", tc.text, tc.limit, got, tc.want)
			}
		})
	}
}

func TestSendOutboundReplyBuildsQuotedContext(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	ownJID := types.NewJID("15550001111", types.DefaultUserServer)
	sender := NewSender(client, &stubBroker{}, ownJID, config.StickerMetadata{}, config.RenderingOptions{})
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
	sender := NewSender(client, &stubBroker{}, ownJID, config.StickerMetadata{}, config.RenderingOptions{})
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

// Inbound chats are canonicalized to a LID (see identityResolver), so a reply
// dispatch targets a `@lid` chat id. whatsmeow sends natively to a LID JID, so
// the dispatch must reach SendMessage with the LID as the destination chat,
// landing the reply in the same conversation the original arrived on.
func TestSendDeliversToCanonicalLIDTarget(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, types.JID{}, config.StickerMetadata{}, config.RenderingOptions{})
	lid := types.NewJID("88887777", types.HiddenUserServer)
	text := "reply to lid chat"
	messageID, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: lid.String(),
		Text:         &text,
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if messageID != "sent-id" {
		t.Fatalf("SendMessage() id = %q", messageID)
	}
	if client.sentTo != lid {
		t.Fatalf("sent to %q, want canonical LID %q", client.sentTo, lid)
	}
}

func TestEditMessageRevisesTargetInPlace(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, types.JID{}, config.StickerMetadata{}, config.RenderingOptions{})
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
	sender := NewSender(client, &stubBroker{}, types.JID{}, config.StickerMetadata{}, config.RenderingOptions{})
	reply := "parent-id"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId:             "123@s.whatsapp.net",
		ReplyToPlatformMessageId: &reply,
	})
	if err == nil || client.sent != nil {
		t.Fatalf("SendMessage() error = %v, sent = %#v", err, client.sent)
	}
}
