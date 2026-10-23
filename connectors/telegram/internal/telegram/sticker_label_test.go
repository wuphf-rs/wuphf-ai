package telegramconnector

import (
	"context"
	"testing"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/bin"
	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/tg"
)

// recordingInvoker answers Telegram API calls with empty successes and records
// every request, so a test can see what a send put on the wire.
type recordingInvoker struct {
	requests []bin.Encoder
	sent     int
}

func (r *recordingInvoker) Invoke(_ context.Context, input bin.Encoder, output bin.Decoder) error {
	r.requests = append(r.requests, input)
	var response bin.Encoder
	switch input.(type) {
	case *tg.UploadSaveFilePartRequest:
		response = &tg.BoolTrue{}
	default:
		// Each sent message gets the next id, as Telegram would assign.
		r.sent++
		response = &tg.UpdateShortSentMessage{ID: r.sent}
	}
	var buffer bin.Buffer
	if err := buffer.Encode(response); err != nil {
		return err
	}
	return output.Decode(&buffer)
}

// sendLabelledSticker sends a static sticker labelled label through a bot or
// user session and returns the API requests it made.
func sendLabelledSticker(t *testing.T, bot bool, label string) []bin.Encoder {
	t.Helper()
	invoker := &recordingInvoker{}
	api := tg.NewClient(invoker)
	sender := Sender{bot: bot}
	builder := message.NewSender(api).To(&tg.InputPeerSelf{}).CloneBuilder()
	mime := "image/webp"
	attachment := &wuphfpb.Attachment{MediaId: "m", Kind: wuphfpb.Attachment_KIND_STICKER, MimeType: &mime}
	if _, err := sender.sendSticker(context.Background(), builder, attachment, []byte("webp"), label); err != nil {
		t.Fatalf("sendSticker() error = %v", err)
	}
	return invoker.requests
}

func sentMedia(t *testing.T, requests []bin.Encoder) *tg.MessagesSendMediaRequest {
	t.Helper()
	for _, request := range requests {
		if media, ok := request.(*tg.MessagesSendMediaRequest); ok {
			return media
		}
	}
	t.Fatalf("no sticker was sent among %d requests", len(requests))
	return nil
}

func TestBotLabelsStickerWithInlineButton(t *testing.T) {
	t.Parallel()
	requests := sendLabelledSticker(t, true, "🗣️: me")
	markup, ok := sentMedia(t, requests).ReplyMarkup.(*tg.ReplyInlineMarkup)
	if !ok || len(markup.Rows) != 1 || len(markup.Rows[0].Buttons) != 1 {
		t.Fatalf("reply markup = %#v, want one inline button", sentMedia(t, requests).ReplyMarkup)
	}
	button, ok := markup.Rows[0].Buttons[0].(*tg.KeyboardButtonCallback)
	if !ok || button.Text != "🗣️: me" || string(button.Data) != labelCallbackData {
		t.Fatalf("button = %#v, want the label with the label callback data", markup.Rows[0].Buttons[0])
	}
	for _, request := range requests {
		if _, ok := request.(*tg.MessagesSendMessageRequest); ok {
			t.Fatal("a bot sent a separate label message")
		}
	}
}

func TestUserSessionLabelsStickerWithRepliedLabelMessage(t *testing.T) {
	t.Parallel()
	requests := sendLabelledSticker(t, false, "🗣️: me")
	label, ok := requests[0].(*tg.MessagesSendMessageRequest)
	if !ok {
		t.Fatalf("first request = %T, want the label message", requests[0])
	}
	if label.Message != "🗣️: me\n(Sticker)" {
		t.Fatalf("label message = %q", label.Message)
	}
	sticker := sentMedia(t, requests)
	if sticker.ReplyMarkup != nil {
		t.Fatalf("a user session attached reply markup %#v", sticker.ReplyMarkup)
	}
	// The label message was the first message sent, so Telegram gave it id 1.
	reply, ok := sticker.ReplyTo.(*tg.InputReplyToMessage)
	if !ok || reply.ReplyToMsgID != 1 {
		t.Fatalf("sticker reply-to = %#v, want the label message (id 1)", sticker.ReplyTo)
	}
}

func TestUnlabelledStickerSendsOnlyTheSticker(t *testing.T) {
	t.Parallel()
	for _, bot := range []bool{true, false} {
		requests := sendLabelledSticker(t, bot, "")
		if sentMedia(t, requests).ReplyMarkup != nil {
			t.Fatalf("bot=%v: unlabelled sticker carries reply markup", bot)
		}
		for _, request := range requests {
			if _, ok := request.(*tg.MessagesSendMessageRequest); ok {
				t.Fatalf("bot=%v: unlabelled sticker sent a label message", bot)
			}
		}
	}
}

func TestSenderLabel(t *testing.T) {
	t.Parallel()
	for _, test := range []struct{ name, participant, want string }{
		{"Alice", "p1", "🗣️: Alice"},
		{"", "p1", "🗣️: p1"},
		{"me", "me", "🗣️: me"},
		{"Alice", "", ""},
	} {
		if got := senderLabel(test.name, test.participant); got != test.want {
			t.Errorf("senderLabel(%q, %q) = %q, want %q", test.name, test.participant, got, test.want)
		}
	}
}
