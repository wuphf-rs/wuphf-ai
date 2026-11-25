package telegramconnector

import (
	"context"
	"strings"
	"sync"
	"testing"

	"github.com/gotd/td/bin"
	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/tg"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/telegram/internal/config"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// copyAPI answers getMessages with copy, uploads with true, and any send or
// edit with a new message, recording every request.
type copyAPI struct {
	mu       sync.Mutex
	copy     *tg.Message
	requests []bin.Encoder
}

func (a *copyAPI) Invoke(_ context.Context, input bin.Encoder, output bin.Decoder) error {
	a.mu.Lock()
	a.requests = append(a.requests, input)
	a.mu.Unlock()
	var response bin.Encoder
	switch input.(type) {
	case *tg.MessagesGetMessagesRequest, *tg.ChannelsGetMessagesRequest:
		messages := &tg.MessagesMessages{}
		if a.copy != nil {
			messages.Messages = []tg.MessageClass{a.copy}
		}
		response = messages
	case *tg.UploadSaveFilePartRequest, *tg.UploadSaveBigFilePartRequest:
		response = &tg.BoolTrue{}
	default:
		response = &tg.Updates{Updates: []tg.UpdateClass{&tg.UpdateNewMessage{
			Message: &tg.Message{ID: 99, PeerID: &tg.PeerChat{ChatID: 5}},
		}}}
	}
	var buffer bin.Buffer
	if err := response.Encode(&buffer); err != nil {
		return err
	}
	return output.Decode(&buffer)
}

func copyRequests[T bin.Encoder](a *copyAPI) []T {
	a.mu.Lock()
	defer a.mu.Unlock()
	var found []T
	for _, request := range a.requests {
		if typed, ok := request.(T); ok {
			found = append(found, typed)
		}
	}
	return found
}

func editingSender(copy *tg.Message, maxLength uint32) (Sender, *copyAPI) {
	api := &copyAPI{copy: copy}
	client := tg.NewClient(api)
	runtime := config.Runtime{}.WithDefaults()
	runtime.Rendering.MaxMessageLength = maxLength
	return Sender{api: client, sender: message.NewSender(client), peers: NewPeerCache(), runtime: settings.NewLive(runtime)}, api
}

func textCopy() *tg.Message {
	return &tg.Message{ID: 7, PeerID: &tg.PeerChat{ChatID: 5}, Message: "short"}
}

func spilledCopy() *tg.Message {
	copy := &tg.Message{ID: 7, PeerID: &tg.PeerChat{ChatID: 5}}
	copy.SetMedia(&tg.MessageMediaDocument{Document: &tg.Document{
		Attributes: []tg.DocumentAttributeClass{&tg.DocumentAttributeFilename{FileName: spillFileName}},
	}})
	return copy
}

func photoCopy() *tg.Message {
	copy := &tg.Message{ID: 7, PeerID: &tg.PeerChat{ChatID: 5}, Message: "caption"}
	copy.SetMedia(&tg.MessageMediaPhoto{})
	return copy
}

func edit(text string) *wuphfpb.MessageEditDispatch {
	return &wuphfpb.MessageEditDispatch{TargetChatId: "telegram:chat:5", TargetPlatformMessageId: "7", Text: &text}
}

// The edit's new media, when it replaced the copy's content.
func replacedMedia(t *testing.T, api *copyAPI) *tg.InputMediaUploadedDocument {
	t.Helper()
	for _, request := range copyRequests[*tg.MessagesEditMessageRequest](api) {
		if media := request.Media; media != nil {
			document, ok := media.(*tg.InputMediaUploadedDocument)
			if !ok {
				t.Fatalf("replaced with %T", media)
			}
			return document
		}
	}
	return nil
}

// Text that still fits is edited as text (plan 086).
func TestAnEditThatFitsEditsTheText(t *testing.T) {
	sender, api := editingSender(textCopy(), 100)
	if _, err := sender.EditMessage(context.Background(), edit("revised")); err != nil {
		t.Fatal(err)
	}
	edits := copyRequests[*tg.MessagesEditMessageRequest](api)
	if len(edits) != 1 || edits[0].Message != "revised" || replacedMedia(t, api) != nil {
		t.Fatalf("edits = %#v", edits)
	}
}

// Text edited past the length limit becomes the file, in place.
func TestTextEditedPastTheLimitBecomesAFile(t *testing.T) {
	sender, api := editingSender(textCopy(), 10)
	if _, err := sender.EditMessage(context.Background(), edit(strings.Repeat("x", 50))); err != nil {
		t.Fatal(err)
	}
	if replacedMedia(t, api) == nil {
		t.Fatalf("not replaced with a file: %#v", api.requests)
	}
	if sends := copyRequests[*tg.MessagesSendMediaRequest](api); len(sends) != 0 {
		t.Fatalf("sent a new message: %#v", sends)
	}
}

// A spilled copy gets a new file even when the edited text is short: Telegram
// cannot turn a file back into text.
func TestASpilledCopyGetsANewFile(t *testing.T) {
	sender, api := editingSender(spilledCopy(), 4096)
	if _, err := sender.EditMessage(context.Background(), edit("now short")); err != nil {
		t.Fatal(err)
	}
	document := replacedMedia(t, api)
	if document == nil || document.MimeType != spillMime {
		t.Fatalf("replaced with %#v", document)
	}
}

// A caption that fits is edited.
func TestAShortCaptionIsEdited(t *testing.T) {
	sender, api := editingSender(photoCopy(), 4096)
	if _, err := sender.EditMessage(context.Background(), edit("new caption")); err != nil {
		t.Fatal(err)
	}
	edits := copyRequests[*tg.MessagesEditMessageRequest](api)
	if len(edits) != 1 || edits[0].Message != "new caption" || replacedMedia(t, api) != nil {
		t.Fatalf("edits = %#v", edits)
	}
}

// A caption past Telegram's limit is cut short, and the full text follows in
// a reply as a file.
func TestALongCaptionIsCutAndTheFullTextFollows(t *testing.T) {
	sender, api := editingSender(photoCopy(), 4096)
	long := strings.Repeat("y", 2000)
	if _, err := sender.EditMessage(context.Background(), edit(long)); err != nil {
		t.Fatal(err)
	}
	edits := copyRequests[*tg.MessagesEditMessageRequest](api)
	if len(edits) != 1 || utf16Len(edits[0].Message) > captionLimit || !strings.HasSuffix(edits[0].Message, "…") {
		t.Fatalf("caption edit (%d) = %q", utf16Len(edits[0].Message), edits[0].Message[max(0, len(edits[0].Message)-20):])
	}
	replies := copyRequests[*tg.MessagesSendMediaRequest](api)
	if len(replies) != 1 {
		t.Fatalf("replies = %#v", replies)
	}
	if reply, ok := replies[0].ReplyTo.(*tg.InputReplyToMessage); !ok || reply.ReplyToMsgID != 7 {
		t.Fatalf("the full text replies to %#v", replies[0].ReplyTo)
	}
}

// A copy that cannot be read back is edited as text, as before.
func TestAnUnreadableCopyIsEditedAsText(t *testing.T) {
	sender, api := editingSender(nil, 4096)
	if _, err := sender.EditMessage(context.Background(), edit("revised")); err != nil {
		t.Fatal(err)
	}
	if edits := copyRequests[*tg.MessagesEditMessageRequest](api); len(edits) != 1 || edits[0].Message != "revised" {
		t.Fatalf("edits = %#v", edits)
	}
}

func TestCutUTF16(t *testing.T) {
	for _, tc := range []struct {
		text  string
		limit int
		want  string
	}{
		{"hello", 3, "hel"},
		{"hello", 10, "hello"},
		{"a😀b", 2, "a"},
		{"a😀b", 3, "a😀"},
		{"x", 0, ""},
	} {
		if got := cutUTF16(tc.text, tc.limit); got != tc.want {
			t.Errorf("cutUTF16(%q, %d) = %q, want %q", tc.text, tc.limit, got, tc.want)
		}
	}
}
