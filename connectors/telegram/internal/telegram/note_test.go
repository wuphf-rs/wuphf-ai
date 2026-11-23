package telegramconnector

import (
	"context"
	"strings"
	"sync"
	"testing"

	"github.com/gotd/td/bin"
	"github.com/gotd/td/tg"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/protobuf/proto"
)

// recordingAPI stands in for Telegram: it records every request and answers
// each send with a new message (an upload part with success), so the real
// message builder runs end to end without a network.
type recordingAPI struct {
	mu       sync.Mutex
	requests []bin.Encoder
	nextID   int
}

func (r *recordingAPI) Invoke(_ context.Context, input bin.Encoder, output bin.Decoder) error {
	r.mu.Lock()
	r.requests = append(r.requests, input)
	r.nextID++
	id := r.nextID
	r.mu.Unlock()
	var response bin.Encoder
	switch input.(type) {
	case *tg.UploadSaveFilePartRequest, *tg.UploadSaveBigFilePartRequest, *tg.AccountUpdateNotifySettingsRequest:
		response = &tg.BoolTrue{}
	default:
		response = &tg.Updates{Updates: []tg.UpdateClass{&tg.UpdateNewMessage{
			Message: &tg.Message{ID: id, PeerID: &tg.PeerChat{ChatID: 42}},
		}}}
	}
	var buffer bin.Buffer
	if err := response.Encode(&buffer); err != nil {
		return err
	}
	return output.Decode(&buffer)
}

// sent returns the recorded requests of type T.
func sent[T bin.Encoder](r *recordingAPI) []T {
	r.mu.Lock()
	defer r.mu.Unlock()
	var found []T
	for _, request := range r.requests {
		if typed, ok := request.(T); ok {
			found = append(found, typed)
		}
	}
	return found
}

type staticMedia struct{ bytes []byte }

func (m staticMedia) Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error) {
	return nil, nil
}

func (m staticMedia) Download(context.Context, string) ([]byte, error) {
	return m.bytes, nil
}

func noteSender(api *recordingAPI) Sender {
	return NewSender(tg.NewClient(api), NewPeerCache(), staticMedia{bytes: []byte("jpeg")}, nil, nil, false)
}

// note is a system note into topic 7 of a basic group, with the fields a note
// must ignore set: a participant, a sender name and a reply.
func note(text string) *wuphfpb.MessageCreateDispatch {
	return &wuphfpb.MessageCreateDispatch{
		DispatchId:               "d-1",
		TargetChatId:             "telegram:chat:42",
		TargetThreadId:           proto.String("7"),
		Participant:              proto.String("123@s.whatsapp.net"),
		SenderName:               proto.String("Alice"),
		ReplyToPlatformMessageId: proto.String("99"),
		Text:                     proto.String(text),
		SystemNote:               true,
	}
}

func replyTo(t *testing.T, reply tg.InputReplyToClass) int {
	t.Helper()
	message, ok := reply.(*tg.InputReplyToMessage)
	if !ok {
		t.Fatalf("reply is %T, want a message reply", reply)
	}
	return message.ReplyToMsgID
}

func TestSendRendersANoteWithoutAHeaderOrAReply(t *testing.T) {
	api := &recordingAPI{}
	if _, _, err := noteSender(api).SendMessage(context.Background(), note("📝 Description changed:\nNew plans")); err != nil {
		t.Fatal(err)
	}
	messages := sent[*tg.MessagesSendMessageRequest](api)
	if len(messages) != 1 {
		t.Fatalf("got %d sends, want 1", len(messages))
	}
	if got, want := messages[0].Message, "📝 Description changed:\nNew plans"; got != want {
		t.Fatalf("text = %q, want %q (no sender header)", got, want)
	}
	for _, entity := range messages[0].Entities {
		if _, bold := entity.(*tg.MessageEntityBold); bold {
			t.Fatalf("a note has no bold sender header: %#v", messages[0].Entities)
		}
	}
	if got := replyTo(t, messages[0].ReplyTo); got != 7 {
		t.Fatalf("a note replies to the topic root 7, not the quoted message: got %d", got)
	}

	// The same dispatch as an ordinary message is labelled and quotes.
	ordinary := note("hello")
	ordinary.SystemNote = false
	if _, _, err := noteSender(api).SendMessage(context.Background(), ordinary); err != nil {
		t.Fatal(err)
	}
	messages = sent[*tg.MessagesSendMessageRequest](api)
	if !strings.HasPrefix(messages[1].Message, "🗣️: Alice") {
		t.Fatalf("an ordinary message keeps its header: %q", messages[1].Message)
	}
	if got := replyTo(t, messages[1].ReplyTo); got != 99 {
		t.Fatalf("an ordinary message quotes: got %d", got)
	}
}

func TestSendRendersANotePhotoWithItsTextAsCaption(t *testing.T) {
	api := &recordingAPI{}
	dispatch := note("🖼️ Photo changed")
	dispatch.Attachments = []*wuphfpb.Attachment{{
		MediaId:  "m-1",
		Kind:     wuphfpb.Attachment_KIND_IMAGE,
		FileName: proto.String("avatar.jpg"),
		MimeType: proto.String("image/jpeg"),
	}}
	if _, _, err := noteSender(api).SendMessage(context.Background(), dispatch); err != nil {
		t.Fatal(err)
	}
	media := sent[*tg.MessagesSendMediaRequest](api)
	if len(media) != 1 {
		t.Fatalf("got %d media sends, want 1", len(media))
	}
	if _, photo := media[0].Media.(*tg.InputMediaUploadedPhoto); !photo {
		t.Fatalf("a note photo goes through the photo path, got %T", media[0].Media)
	}
	if got, want := media[0].Message, "🖼️ Photo changed"; got != want {
		t.Fatalf("caption = %q, want %q", got, want)
	}
	if got := replyTo(t, media[0].ReplyTo); got != 7 {
		t.Fatalf("a note photo replies to the topic root, got %d", got)
	}
}

func TestANoteCanProvisionItsTopic(t *testing.T) {
	api := &recordingAPI{}
	dispatch := note("ℹ️ [G] Family")
	dispatch.TargetThreadId = nil
	dispatch.ProvisionThread = &wuphfpb.ProvisionThread{Container: "telegram:chat:42", Title: "[G] Family"}

	messageID, threadID, err := noteSender(api).SendMessage(context.Background(), dispatch)
	if err != nil {
		t.Fatal(err)
	}
	topics := sent[*tg.MessagesCreateForumTopicRequest](api)
	if len(topics) != 1 || topics[0].Title != "[G] Family" {
		t.Fatalf("one topic named after the chat, got %#v", topics)
	}
	// The fake numbers responses in order: the topic is 1, the card 2.
	if threadID != "1" || messageID != "2" {
		t.Fatalf("got message %q in thread %q, want 2 in 1", messageID, threadID)
	}
	messages := sent[*tg.MessagesSendMessageRequest](api)
	if got := replyTo(t, messages[0].ReplyTo); got != 1 {
		t.Fatalf("the card is posted into the new topic, got reply to %d", got)
	}
}
