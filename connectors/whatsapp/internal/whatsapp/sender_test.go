package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"testing"

	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/appstate"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"
)

type stubSendClient struct {
	sent       *waE2E.Message
	sentTo     types.JID
	uploaded   []byte
	media      whatsmeow.MediaType
	editedKey  types.MessageID
	revokedKey types.MessageID
	patches    []appstate.PatchInfo
	reaction   *stubReaction
	// newsletterUpload: the last upload was a channel's (unencrypted).
	newsletterUpload bool
	extra            []whatsmeow.SendRequestExtra
}

// stubReaction is what BuildReaction was asked for.
type stubReaction struct {
	chat, author types.JID
	id           types.MessageID
	emoji        string
}

func (s *stubSendClient) SendMessage(_ context.Context, to types.JID, message *waE2E.Message, extra ...whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
	s.extra = extra
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

func (s *stubSendClient) BuildReaction(chat, sender types.JID, id types.MessageID, reaction string) *waE2E.Message {
	s.reaction = &stubReaction{chat: chat, author: sender, id: id, emoji: reaction}
	return &waE2E.Message{ReactionMessage: &waE2E.ReactionMessage{Text: proto.String(reaction)}}
}

func (s *stubSendClient) SendAppState(_ context.Context, patch appstate.PatchInfo) error {
	s.patches = append(s.patches, patch)
	return nil
}

func (s *stubSendClient) Upload(_ context.Context, bytes []byte, media whatsmeow.MediaType) (whatsmeow.UploadResponse, error) {
	s.uploaded, s.media = bytes, media
	return whatsmeow.UploadResponse{URL: "url", DirectPath: "path", FileLength: uint64(len(bytes))}, nil
}

func (s *stubSendClient) UploadNewsletter(_ context.Context, bytes []byte, media whatsmeow.MediaType) (whatsmeow.UploadResponse, error) {
	s.uploaded, s.media, s.newsletterUpload = bytes, media, true
	return whatsmeow.UploadResponse{URL: "url", DirectPath: "path", FileLength: uint64(len(bytes)), Handle: "handle-1"}, nil
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
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
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
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{AttachmentsAsFiles: true}))
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
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{MaxMessageLength: 5}))
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
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{MaxMessageLength: 5}))
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
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{ownPN: ownJID}, liveRendering(config.RenderingOptions{}))
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
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{ownPN: ownJID}, liveRendering(config.RenderingOptions{}))
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
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
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
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
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
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	reply := "parent-id"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId:             "123@s.whatsapp.net",
		ReplyToPlatformMessageId: &reply,
	})
	if err == nil || client.sent != nil {
		t.Fatalf("SendMessage() error = %v, sent = %#v", err, client.sent)
	}
}

// recordingSendClient keeps every message it was asked to send, and can fail on
// a chosen call so a partly-delivered attachment group can be exercised.
type recordingSendClient struct {
	stubSendClient
	sentAll []*waE2E.Message
	failAt  int
	err     error
}

func (r *recordingSendClient) SendMessage(ctx context.Context, to types.JID, message *waE2E.Message, extra ...whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
	if r.err != nil && len(r.sentAll) == r.failAt {
		return whatsmeow.SendResponse{}, r.err
	}
	r.sentAll = append(r.sentAll, message)
	response, err := r.stubSendClient.SendMessage(ctx, to, message, extra...)
	response.ID = types.MessageID(fmt.Sprintf("sent-%d", len(r.sentAll)))
	return response, err
}

func imageAttachments(n int) []*wuphfpb.Attachment {
	attachments := make([]*wuphfpb.Attachment, 0, n)
	for i := 0; i < n; i++ {
		mime := "image/jpeg"
		attachments = append(attachments, &wuphfpb.Attachment{
			MediaId:  fmt.Sprintf("media-%d", i),
			Kind:     wuphfpb.Attachment_KIND_IMAGE,
			MimeType: &mime,
		})
	}
	return attachments
}

func TestSendMultipleAttachmentsCaptionsOnlyTheFirst(t *testing.T) {
	t.Parallel()
	client := &recordingSendClient{}
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	text := "caption"
	id, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
		Attachments:  imageAttachments(3),
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if len(client.sentAll) != 3 {
		t.Fatalf("sent %d messages, want one per attachment", len(client.sentAll))
	}
	if got := client.sentAll[0].GetImageMessage().GetCaption(); got != text {
		t.Fatalf("first caption = %q, want %q", got, text)
	}
	for i, sent := range client.sentAll[1:] {
		if caption := sent.GetImageMessage().GetCaption(); caption != "" {
			t.Fatalf("attachment %d caption = %q, want the caption only on the first", i+1, caption)
		}
	}
	// The copy the core records is the head of the group, so replies and edits
	// against this message resolve to the first item.
	if id != "sent-1" {
		t.Fatalf("recorded copy = %q, want the first message id", id)
	}
}

func TestSendMultipleAttachmentsDeadLettersAfterPartialDelivery(t *testing.T) {
	t.Parallel()
	client := &recordingSendClient{failAt: 1, err: errors.New("upstream refused")}
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Attachments:  imageAttachments(3),
	})
	if err == nil {
		t.Fatal("SendMessage() error = nil, want the partial-group failure")
	}
	// Permanent so the core dead-letters instead of redelivering, which would
	// duplicate the attachments that did go out.
	var permanent coreclient.PermanentError
	if !errors.As(err, &permanent) {
		t.Fatalf("SendMessage() error = %v, want a permanent error", err)
	}
	if len(client.sentAll) != 1 {
		t.Fatalf("sent %d messages, want only the one before the failure", len(client.sentAll))
	}
}

func TestSendFirstAttachmentFailureStaysRetryable(t *testing.T) {
	t.Parallel()
	client := &recordingSendClient{failAt: 0, err: errors.New("transient")}
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Attachments:  imageAttachments(2),
	})
	var permanent coreclient.PermanentError
	if err == nil || errors.As(err, &permanent) {
		t.Fatalf("SendMessage() error = %v, want a retryable failure with nothing sent", err)
	}
}

func TestMessageAuthorAttributesTheRealSender(t *testing.T) {
	t.Parallel()
	own := types.NewJID("19998887777", types.DefaultUserServer)
	peer := "88887777@lid"

	cases := []struct {
		name  string
		reply *wuphfpb.ReplyContext
		want  string
	}{
		{
			// The bridge wrote the quoted copy, so WhatsApp is told we authored it.
			"bridge-authored copy quotes our own account",
			&wuphfpb.ReplyContext{AuthoredByBridge: true, Sender: peer, Participant: &peer},
			own.String(),
		},
		{
			// The copy came from WhatsApp, so its author is the group participant.
			"received copy quotes the participant",
			&wuphfpb.ReplyContext{Sender: "123@g.us", Participant: &peer},
			peer,
		},
		{
			"received copy without a participant falls back to the sender",
			&wuphfpb.ReplyContext{Sender: peer},
			peer,
		},
		{
			"a received copy with no author at all still sends",
			&wuphfpb.ReplyContext{},
			own.String(),
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			sender := NewSender(&stubSendClient{}, &stubBroker{}, stubConverter{}, staticAddressing{ownPN: own}, liveRendering(config.RenderingOptions{}))
			got, err := sender.messageAuthor(context.Background(), tc.reply.GetAuthoredByBridge(), tc.reply.GetParticipant(), tc.reply.GetSender(), "")
			if err != nil {
				t.Fatalf("messageAuthor() error = %v", err)
			}
			if got != tc.want {
				t.Fatalf("messageAuthor() = %q, want %q", got, tc.want)
			}
		})
	}
}

// stubConverter fails every conversion; senders under test that never convert
// use it.
type stubConverter struct{}

func (stubConverter) Convert(context.Context, string, string, string, string) ([]byte, error) {
	return nil, errors.New("no converter in this test")
}

// recordingConverter records each conversion request and answers with fixed
// bytes or an error.
type recordingConverter struct {
	calls  []convertCall
	output []byte
	err    error
}

type convertCall struct {
	mediaID, sourceMime, sourceName, targetMime string
}

func (r *recordingConverter) Convert(_ context.Context, mediaID, sourceMime, sourceName, targetMime string) ([]byte, error) {
	r.calls = append(r.calls, convertCall{mediaID, sourceMime, sourceName, targetMime})
	return r.output, r.err
}

// sendSticker dispatches one animated sticker with the given source type and
// returns the message WhatsApp was sent.
func sendSticker(t *testing.T, converter StickerConverter, mimeType, fileName string, animated bool) *waE2E.Message {
	t.Helper()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{bytes: []byte("source")}, converter, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Attachments: []*wuphfpb.Attachment{{
			MediaId:  "media-1",
			Kind:     wuphfpb.Attachment_KIND_STICKER,
			MimeType: &mimeType,
			FileName: &fileName,
			Animated: animated,
		}},
	}); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	return client.sent
}

func TestAnimatedStickerIsConvertedToWebP(t *testing.T) {
	t.Parallel()
	converter := &recordingConverter{output: []byte("animated webp")}
	sent := sendSticker(t, converter, "video/webm", "sticker.webm", true)
	want := []convertCall{{"media-1", "video/webm", "sticker.webm", "image/webp"}}
	if !slices.Equal(converter.calls, want) {
		t.Fatalf("converter calls = %+v, want %+v", converter.calls, want)
	}
	if got := sent.GetStickerMessage().GetMimetype(); got != "image/webp" {
		t.Fatalf("sticker mimetype = %q, want image/webp (message %v)", got, sent)
	}
}

func TestTGSStickerIsConvertedToWebP(t *testing.T) {
	t.Parallel()
	converter := &recordingConverter{output: []byte("animated webp")}
	sent := sendSticker(t, converter, "application/x-tgsticker", "sticker.tgs", true)
	if len(converter.calls) != 1 || converter.calls[0].sourceMime != "application/x-tgsticker" {
		t.Fatalf("converter calls = %+v, want one with the Lottie source mime", converter.calls)
	}
	if sent.GetStickerMessage() == nil {
		t.Fatalf("sent %v, want a sticker", sent)
	}
}

func TestStickerConversionFailureFallsBackToDocument(t *testing.T) {
	t.Parallel()
	converter := &recordingConverter{err: errors.New("no provider for media.convert@1 connected")}
	sent := sendSticker(t, converter, "video/webm", "sticker.webm", true)
	if sent.GetDocumentMessage() == nil || sent.GetStickerMessage() != nil {
		t.Fatalf("sent %v, want the original as a document", sent)
	}
	if got := sent.GetDocumentMessage().GetFileName(); got != "sticker.webm" {
		t.Fatalf("document name = %q, want the source name", got)
	}
}

func TestStaticWebPStickerSkipsConversion(t *testing.T) {
	t.Parallel()
	converter := &recordingConverter{}
	for _, animated := range []bool{false, true} {
		sent := sendSticker(t, converter, "image/webp", "sticker.webp", animated)
		if sent.GetStickerMessage() == nil {
			t.Fatalf("animated=%v: sent %v, want a sticker", animated, sent)
		}
	}
	if len(converter.calls) != 0 {
		t.Fatalf("converter called %d times, want 0 for a WebP source", len(converter.calls))
	}
}

// staticAddressing addresses every chat in one form, translating addresses
// through a fixed phone/LID table. The zero value leaves every address as it is
// and owns the empty JID.
type staticAddressing struct {
	mode          types.AddressingMode
	ownPN, ownLID types.JID
	// counterparts maps an address to its other form, in both directions.
	counterparts map[types.JID]types.JID
}

func (a staticAddressing) Mode(context.Context, types.JID) types.AddressingMode { return a.mode }

func (a staticAddressing) InMode(_ context.Context, jid types.JID, mode types.AddressingMode) types.JID {
	other, ok := a.counterparts[jid]
	switch {
	case ok && mode == types.AddressingModeLID && jid.Server == types.DefaultUserServer,
		ok && mode == types.AddressingModePN && jid.Server == types.HiddenUserServer:
		return other
	}
	return jid
}

func (a staticAddressing) Own(mode types.AddressingMode) types.JID {
	if mode == types.AddressingModeLID && !a.ownLID.IsEmpty() {
		return a.ownLID
	}
	return a.ownPN
}

// liveRendering is runtime settings holding rendering, as the connector's
// settings would after the core pushed them.
func liveRendering(rendering config.RenderingOptions) *settings.Live[config.Runtime] {
	return settings.NewLive(config.Runtime{Rendering: rendering})
}

func dayTimer() *wuphfpb.DisappearingTimer {
	at := int64(1700000000)
	return &wuphfpb.DisappearingTimer{Seconds: 86400, SettingTimestamp: &at}
}

// contentContext is the ContextInfo of whatever content a message carries.
func contentContext(message *waE2E.Message) *waE2E.ContextInfo {
	switch {
	case message.GetExtendedTextMessage() != nil:
		return message.GetExtendedTextMessage().GetContextInfo()
	case message.GetImageMessage() != nil:
		return message.GetImageMessage().GetContextInfo()
	case message.GetDocumentMessage() != nil:
		return message.GetDocumentMessage().GetContextInfo()
	case message.GetVideoMessage() != nil:
		return message.GetVideoMessage().GetContextInfo()
	case message.GetAudioMessage() != nil:
		return message.GetAudioMessage().GetContextInfo()
	case message.GetStickerMessage() != nil:
		return message.GetStickerMessage().GetContextInfo()
	default:
		return nil
	}
}

func assertStamped(t *testing.T, what string, message *waE2E.Message) {
	t.Helper()
	context := contentContext(message)
	if context.GetExpiration() != 86400 || context.GetEphemeralSettingTimestamp() != 1700000000 {
		t.Fatalf("%s: context = %v, want expiration 86400 set at 1700000000 (message %v)", what, context, message)
	}
}

func TestSendStampsTheTimerOnText(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	text := "hi"
	if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
		Disappearing: dayTimer(),
	}); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if client.sent.Conversation != nil || client.sent.GetExtendedTextMessage().GetText() != "hi" {
		t.Fatalf("sent %v, want the text promoted to an extended text message", client.sent)
	}
	assertStamped(t, "text", client.sent)
}

func TestSendStampsTheTimerOnEveryAttachmentKind(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		kind wuphfpb.Attachment_Kind
		mime string
		name string
	}{
		{wuphfpb.Attachment_KIND_IMAGE, "image/jpeg", "photo.jpg"},
		{wuphfpb.Attachment_KIND_FILE, "application/pdf", "receipt.pdf"},
		{wuphfpb.Attachment_KIND_VIDEO, "video/mp4", "clip.mp4"},
		{wuphfpb.Attachment_KIND_AUDIO, "audio/ogg", "voice.ogg"},
		{wuphfpb.Attachment_KIND_STICKER, "image/webp", "sticker.webp"},
	} {
		client := &stubSendClient{}
		sender := NewSender(client, &stubBroker{bytes: []byte("data")}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
		mime, name := c.mime, c.name
		if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
			TargetChatId: "123@s.whatsapp.net",
			Attachments:  []*wuphfpb.Attachment{{MediaId: "media-1", Kind: c.kind, MimeType: &mime, FileName: &name}},
			Disappearing: dayTimer(),
		}); err != nil {
			t.Fatalf("%v: SendMessage() error = %v", c.kind, err)
		}
		assertStamped(t, c.kind.String(), client.sent)
	}

	// Over-length text goes out as a file, stamped the same way.
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{MaxMessageLength: 5}))
	text := "hello world"
	if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
		Disappearing: dayTimer(),
	}); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	assertStamped(t, "text as a file", client.sent)
}

func TestSendStampsEveryPartOfAMultiAttachmentSend(t *testing.T) {
	t.Parallel()
	client := &recordingSendClient{}
	ownJID := types.NewJID("15550001111", types.DefaultUserServer)
	sender := NewSender(client, &stubBroker{bytes: []byte("data")}, stubConverter{}, staticAddressing{ownPN: ownJID}, liveRendering(config.RenderingOptions{}))
	parent := "parent"
	if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Attachments:  imageAttachments(3),
		ReplyContext: &wuphfpb.ReplyContext{PlatformMessageId: "parent-id", Text: &parent},
		Disappearing: dayTimer(),
	}); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if len(client.sentAll) != 3 {
		t.Fatalf("sent %d messages, want 3", len(client.sentAll))
	}
	for i, sent := range client.sentAll {
		assertStamped(t, fmt.Sprintf("part %d", i+1), sent)
		quoted := contentContext(sent).GetStanzaID()
		if (i == 0) != (quoted == "parent-id") {
			t.Fatalf("part %d quotes %q, want the reply context on the first part only", i+1, quoted)
		}
	}
}

func TestSendMergesTheTimerIntoAReplyContext(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	ownJID := types.NewJID("15550001111", types.DefaultUserServer)
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{ownPN: ownJID}, liveRendering(config.RenderingOptions{}))
	text, parent := "reply", "parent"
	if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "123@s.whatsapp.net",
		Text:         &text,
		ReplyContext: &wuphfpb.ReplyContext{PlatformMessageId: "parent-id", Text: &parent},
		Disappearing: dayTimer(),
	}); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	assertStamped(t, "reply", client.sent)
	context := contentContext(client.sent)
	if context.GetStanzaID() != "parent-id" || context.GetParticipant() != ownJID.String() {
		t.Fatalf("reply context = %v, want the quote kept", context)
	}
}

func TestSendWithAnOffOrMissingTimerSetsNoExpiration(t *testing.T) {
	t.Parallel()
	for _, timer := range []*wuphfpb.DisappearingTimer{nil, {Seconds: 0}} {
		client := &stubSendClient{}
		sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
		text := "hi"
		if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
			TargetChatId: "123@s.whatsapp.net",
			Text:         &text,
			Disappearing: timer,
		}); err != nil {
			t.Fatalf("SendMessage() error = %v", err)
		}
		if client.sent.GetConversation() != "hi" || contentContext(client.sent).GetExpiration() != 0 {
			t.Fatalf("timer %v: sent %v, want plain text without an expiration", timer, client.sent)
		}
	}
}

func TestEditAndDeleteCarryNoTimer(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{})).
		WithTimers(newTimerSource(&stubGroups{}, inputs, context.Background()))
	text := "revised"
	if _, err := sender.EditMessage(context.Background(), &wuphfpb.MessageEditDispatch{
		TargetChatId:            "123@s.whatsapp.net",
		TargetPlatformMessageId: "orig-id",
		Text:                    &text,
	}); err != nil {
		t.Fatalf("EditMessage() error = %v", err)
	}
	if client.sent.GetConversation() != "revised" || contentContext(client.sent).GetExpiration() != 0 {
		t.Fatalf("edit sent %v, want plain text without an expiration", client.sent)
	}
	if _, err := sender.DeleteMessage(context.Background(), &wuphfpb.MessageDeleteDispatch{
		TargetChatId:            "123@s.whatsapp.net",
		TargetPlatformMessageId: "orig-id",
	}); err != nil {
		t.Fatalf("DeleteMessage() error = %v", err)
	}
	if contentContext(client.sent).GetExpiration() != 0 {
		t.Fatalf("delete sent %v, want no expiration", client.sent)
	}
	// Neither consulted the timer source: no lookup, no notice.
	select {
	case frame := <-inputs:
		t.Fatalf("an edit or delete emitted %v", frame)
	default:
	}
}

func TestSendRendersASystemNote(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	text := "📝 Description changed:\nNew plans"
	if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId:             "123@s.whatsapp.net",
		Text:                     &text,
		SystemNote:               true,
		ReplyToPlatformMessageId: proto.String("quoted"),
		Disappearing:             dayTimer(),
	}); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if client.sent.GetExtendedTextMessage().GetText() != text {
		t.Fatalf("sent %v, want the note text", client.sent)
	}
	if context := contentContext(client.sent); context.GetStanzaID() != "" || context.GetQuotedMessage() != nil {
		t.Fatalf("a note quotes nothing, got %v", context)
	}
	assertStamped(t, "note", client.sent)
}

// The owner's reaction is sent as this account's, naming the reacted message
// by its author: the participant for a received group message, our own
// account for a copy the bridge sent, the peer in a DM.
func TestApplyReactionNamesTheReactedMessagesAuthor(t *testing.T) {
	t.Parallel()
	own := types.NewJID("19998887777", types.DefaultUserServer)
	participant := "88887777@lid"
	cases := []struct {
		name     string
		dispatch *wuphfpb.ReactionDispatch
		chat     string
		author   string
	}{
		{
			"a received group message",
			&wuphfpb.ReactionDispatch{TargetChatId: "123@g.us", TargetSender: "123@g.us", TargetParticipant: &participant},
			"123@g.us", participant,
		},
		{
			"a copy the bridge sent",
			&wuphfpb.ReactionDispatch{TargetChatId: "123@g.us", TargetSender: "alice", TargetAuthoredByBridge: true},
			"123@g.us", own.String(),
		},
		{
			"a DM",
			&wuphfpb.ReactionDispatch{TargetChatId: "15550001111@s.whatsapp.net", TargetSender: "15550001111@s.whatsapp.net"},
			"15550001111@s.whatsapp.net", "15550001111@s.whatsapp.net",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			client := &stubSendClient{}
			sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{ownPN: own}, liveRendering(config.RenderingOptions{}))
			tc.dispatch.ByOwner = true
			tc.dispatch.TargetPlatformMessageId = "wa-1"
			tc.dispatch.Emoji = "❤️"
			id, err := sender.ApplyReaction(context.Background(), tc.dispatch)
			if err != nil || id != "wa-1" {
				t.Fatalf("ApplyReaction() = %q, %v", id, err)
			}
			got := client.reaction
			if got == nil || got.chat.String() != tc.chat || got.author.String() != tc.author || got.id != "wa-1" || got.emoji != "❤️" {
				t.Fatalf("reaction = %+v, want %s by %s", got, tc.chat, tc.author)
			}
			if client.sent.GetReactionMessage() == nil || client.sentTo.String() != tc.chat {
				t.Fatalf("sent %v to %s", client.sent, client.sentTo)
			}
		})
	}
}

// An empty emoji removes the owner's reaction.
func TestApplyReactionClearsWithAnEmptyEmoji(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	_, err := sender.ApplyReaction(context.Background(), &wuphfpb.ReactionDispatch{
		TargetChatId: "15550001111@s.whatsapp.net", TargetSender: "15550001111@s.whatsapp.net",
		TargetPlatformMessageId: "wa-1", ByOwner: true,
	})
	if err != nil || client.reaction == nil || client.reaction.emoji != "" {
		t.Fatalf("reaction = %+v, %v; want a removal", client.reaction, err)
	}
}

// Someone else's reaction is not the owner's to make: nothing is sent, and
// the dispatch is acknowledged.
func TestApplyReactionSendsNothingForSomeoneElse(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	id, err := sender.ApplyReaction(context.Background(), &wuphfpb.ReactionDispatch{
		TargetChatId: "123@g.us", TargetPlatformMessageId: "wa-1", Emoji: "😂", Reactor: "stranger",
	})
	if err != nil || id != "wa-1" || client.sent != nil || client.reaction != nil {
		t.Fatalf("ApplyReaction() = %q, %v; sent %v", id, err, client.sent)
	}
}
