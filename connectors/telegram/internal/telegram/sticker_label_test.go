package telegramconnector

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/bin"
	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/tg"
	"github.com/rs/zerolog"
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
	if _, err := sender.sendSticker(context.Background(), builder, sendTarget{peer: &tg.InputPeerSelf{}}, attachment, []byte("webp"), label); err != nil {
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

// The inline route: a user session sends a labelled sticker as the helper
// bot's inline result. These fakes stand in for both clients, and the user's
// fake plays Telegram by handing its inline query to the helper's dispatcher.

const stickerOwner = int64(1001)

// helperInvoker answers the helper bot's calls. With failUpload its
// messages.uploadMedia fails; with blockUpload it waits for the caller's
// deadline instead of answering.
type helperInvoker struct {
	mu          sync.Mutex
	requests    []bin.Encoder
	failUpload  bool
	blockUpload bool
}

func (h *helperInvoker) Invoke(ctx context.Context, input bin.Encoder, output bin.Decoder) error {
	h.mu.Lock()
	h.requests = append(h.requests, input)
	h.mu.Unlock()
	var response bin.Encoder
	switch input.(type) {
	case *tg.MessagesUploadMediaRequest:
		if h.blockUpload {
			<-ctx.Done()
			return ctx.Err()
		}
		if h.failUpload {
			return errors.New("PEER_ID_INVALID")
		}
		response = &tg.MessageMediaDocument{Document: &tg.Document{ID: 7, AccessHash: 8, FileReference: []byte{1}}}
	default:
		response = &tg.BoolTrue{}
	}
	return encodeResponse(response, output)
}

// answered is the helper's last inline answer, if any.
func (h *helperInvoker) answered() *tg.MessagesSetInlineBotResultsRequest {
	h.mu.Lock()
	defer h.mu.Unlock()
	for i := len(h.requests) - 1; i >= 0; i-- {
		if answer, ok := h.requests[i].(*tg.MessagesSetInlineBotResultsRequest); ok {
			return answer
		}
	}
	return nil
}

// userInvoker answers the user session's calls. Its inline query is handed to
// the helper's dispatcher, as Telegram would, and the answer comes back as the
// helper gave it.
type userInvoker struct {
	requests   []bin.Encoder
	sent       int
	helper     *helperInvoker
	dispatcher tg.UpdateDispatcher
}

func (u *userInvoker) Invoke(ctx context.Context, input bin.Encoder, output bin.Decoder) error {
	u.requests = append(u.requests, input)
	var response bin.Encoder
	switch request := input.(type) {
	case *tg.UploadSaveFilePartRequest:
		response = &tg.BoolTrue{}
	case *tg.MessagesGetInlineBotResultsRequest:
		query := &tg.UpdateBotInlineQuery{QueryID: 1, UserID: stickerOwner, Query: request.Query}
		updates := &tg.Updates{
			Updates: []tg.UpdateClass{query},
			Users:   []tg.UserClass{&tg.User{ID: stickerOwner, AccessHash: 9}},
		}
		if err := u.dispatcher.Handle(ctx, updates); err != nil {
			return err
		}
		results := &tg.MessagesBotResults{QueryID: 1}
		if answer := u.helper.answered(); answer != nil {
			for _, result := range answer.Results {
				results.Results = append(results.Results, &tg.BotInlineMediaResult{
					ID:          result.(*tg.InputBotInlineResultDocument).ID,
					Type:        "sticker",
					SendMessage: &tg.BotInlineMessageMediaAuto{},
				})
			}
		}
		response = results
	case *tg.MessagesSendInlineBotResultRequest:
		response = &tg.UpdateShortSentMessage{ID: 77}
	default:
		u.sent++
		response = &tg.UpdateShortSentMessage{ID: u.sent}
	}
	return encodeResponse(response, output)
}

func encodeResponse(response bin.Encoder, output bin.Decoder) error {
	var buffer bin.Buffer
	if err := buffer.Encode(response); err != nil {
		return err
	}
	return output.Decode(&buffer)
}

// inlineFixture wires a user session's sender to a helper bot.
type inlineFixture struct {
	user     *userInvoker
	helper   *helperInvoker
	stickers *InlineStickers
	sender   Sender
}

func newInlineFixture(helper *helperInvoker) inlineFixture {
	helperAPI := tg.NewClient(helper)
	stickers := NewInlineStickers(zerolog.Nop())
	user := &userInvoker{
		helper:     helper,
		dispatcher: newHelperDispatcher(NewPanel("wuphf", NewPeerCache(), zerolog.Nop()), stickers, nil, func() *tg.Client { return helperAPI }),
	}
	userAPI := tg.NewClient(user)
	stickers.attach(userAPI, helperAPI, &tg.InputUser{UserID: 2002, AccessHash: 3}, stickerOwner)
	return inlineFixture{
		user:     user,
		helper:   helper,
		stickers: stickers,
		sender:   Sender{api: userAPI, sender: message.NewSender(userAPI)}.WithInlineStickers(stickers),
	}
}

// sendLabelled sends a labelled static sticker from the user session,
// replying to message 5.
func (f inlineFixture) sendLabelled(t *testing.T) tg.UpdatesClass {
	t.Helper()
	builder := f.sender.sender.To(&tg.InputPeerSelf{}).CloneBuilder().Reply(5)
	mime := "image/webp"
	attachment := &wuphfpb.Attachment{MediaId: "m", Kind: wuphfpb.Attachment_KIND_STICKER, MimeType: &mime}
	updates, err := f.sender.sendSticker(context.Background(), builder, sendTarget{peer: &tg.InputPeerSelf{}, replyTo: 5}, attachment, []byte("webp"), "🗣️: Alice")
	if err != nil {
		t.Fatalf("sendSticker() error = %v", err)
	}
	return updates
}

// queryHelper hands the helper's dispatcher one inline query from user.
func (f inlineFixture) queryHelper(t *testing.T, user int64, query string) {
	t.Helper()
	updates := &tg.Updates{
		Updates: []tg.UpdateClass{&tg.UpdateBotInlineQuery{QueryID: 1, UserID: user, Query: query}},
		Users:   []tg.UserClass{&tg.User{ID: user, AccessHash: 9}},
	}
	if err := f.user.dispatcher.Handle(context.Background(), updates); err != nil {
		t.Fatalf("helper inline query error = %v", err)
	}
}

// stage puts a sticker under nonce, as Send does before asking the helper.
func (f inlineFixture) stage(nonce string) {
	f.stickers.mu.Lock()
	defer f.stickers.mu.Unlock()
	f.stickers.pending[nonce] = pendingSticker{data: []byte("webp"), mime: "image/webp", filename: "sticker.webp", label: "🗣️: Alice", expires: time.Now().Add(time.Minute)}
}

func TestHelperAnswersStickerQueryWithLabelButton(t *testing.T) {
	t.Parallel()
	f := newInlineFixture(&helperInvoker{})
	f.stage(stickerNoncePrefix + "a")
	f.queryHelper(t, stickerOwner, stickerNoncePrefix+"a")

	answer := f.helper.answered()
	if answer == nil || len(answer.Results) != 1 {
		t.Fatalf("answer = %#v, want one result", answer)
	}
	result, ok := answer.Results[0].(*tg.InputBotInlineResultDocument)
	if !ok || result.Type != "sticker" {
		t.Fatalf("result = %#v, want a sticker document", answer.Results[0])
	}
	media, ok := result.SendMessage.(*tg.InputBotInlineMessageMediaAuto)
	if !ok {
		t.Fatalf("send message = %#v", result.SendMessage)
	}
	markup, ok := media.ReplyMarkup.(*tg.ReplyInlineMarkup)
	if !ok || len(markup.Rows) != 1 || len(markup.Rows[0].Buttons) != 1 {
		t.Fatalf("reply markup = %#v, want one button", media.ReplyMarkup)
	}
	button, ok := markup.Rows[0].Buttons[0].(*tg.KeyboardButtonCallback)
	if !ok || button.Text != "🗣️: Alice" || string(button.Data) != labelCallbackData {
		t.Fatalf("button = %#v, want the label with the label callback data", markup.Rows[0].Buttons[0])
	}
}

func TestHelperIgnoresUnknownNonceOrOtherUser(t *testing.T) {
	t.Parallel()
	f := newInlineFixture(&helperInvoker{})
	f.stage(stickerNoncePrefix + "a")

	f.queryHelper(t, 4242, stickerNoncePrefix+"a")
	if answer := f.helper.answered(); answer == nil || len(answer.Results) != 0 {
		t.Fatalf("a stranger got %#v, want an empty answer", answer)
	}
	f.queryHelper(t, stickerOwner, stickerNoncePrefix+"unknown")
	if answer := f.helper.answered(); answer == nil || len(answer.Results) != 0 {
		t.Fatalf("an unknown nonce got %#v, want an empty answer", answer)
	}
	// The stranger did not use the account's nonce up.
	f.queryHelper(t, stickerOwner, stickerNoncePrefix+"a")
	if answer := f.helper.answered(); answer == nil || len(answer.Results) != 1 {
		t.Fatalf("the account got %#v after a stranger's query, want the sticker", answer)
	}
}

func TestNonceIsSingleUse(t *testing.T) {
	t.Parallel()
	f := newInlineFixture(&helperInvoker{})
	f.stage(stickerNoncePrefix + "a")
	f.queryHelper(t, stickerOwner, stickerNoncePrefix+"a")
	f.queryHelper(t, stickerOwner, stickerNoncePrefix+"a")
	if answer := f.helper.answered(); answer == nil || len(answer.Results) != 0 {
		t.Fatalf("a reused nonce got %#v, want an empty answer", answer)
	}
}

func TestHelperAnswersLabelPressSilently(t *testing.T) {
	t.Parallel()
	f := newInlineFixture(&helperInvoker{})
	press := &tg.UpdateInlineBotCallbackQuery{QueryID: 33, UserID: 4242, Data: []byte(labelCallbackData)}
	if err := f.user.dispatcher.Handle(context.Background(), &tg.Updates{Updates: []tg.UpdateClass{press}}); err != nil {
		t.Fatalf("press error = %v", err)
	}
	var answer *tg.MessagesSetBotCallbackAnswerRequest
	for _, request := range f.helper.requests {
		if found, ok := request.(*tg.MessagesSetBotCallbackAnswerRequest); ok {
			answer = found
		}
	}
	if answer == nil || answer.QueryID != 33 || answer.Message != "" || answer.Alert {
		t.Fatalf("press answer = %#v, want a silent answer to query 33", answer)
	}
}

func TestUserSessionStickerGoesThroughHelper(t *testing.T) {
	t.Parallel()
	f := newInlineFixture(&helperInvoker{})
	updates := f.sendLabelled(t)

	if len(f.user.requests) != 2 {
		t.Fatalf("user requests = %s, want the inline query and its send only", requestTypes(f.user.requests))
	}
	if _, ok := f.user.requests[0].(*tg.MessagesGetInlineBotResultsRequest); !ok {
		t.Fatalf("first user request = %T, want the inline query", f.user.requests[0])
	}
	send, ok := f.user.requests[1].(*tg.MessagesSendInlineBotResultRequest)
	if !ok || send.ID != inlineStickerResultID {
		t.Fatalf("second user request = %#v, want the sticker result sent", f.user.requests[1])
	}
	if reply, ok := send.ReplyTo.(*tg.InputReplyToMessage); !ok || reply.ReplyToMsgID != 5 {
		t.Fatalf("reply-to = %#v, want message 5", send.ReplyTo)
	}
	uploaded := false
	for _, request := range f.helper.requests {
		if _, ok := request.(*tg.MessagesUploadMediaRequest); ok {
			uploaded = true
		}
	}
	if !uploaded {
		t.Fatal("the helper did not upload the sticker")
	}
	// The recorded copy is the sent sticker itself.
	if id, err := sentMessageID(updates); err != nil || id != 77 {
		t.Fatalf("copy id = %d (%v), want the sent sticker (77)", id, err)
	}
	// Answered nonces are gone.
	if len(f.stickers.pending) != 0 {
		t.Fatalf("pending = %d, want none", len(f.stickers.pending))
	}
}

// assertLabelFallback checks the user session sent today's label message and
// replied the sticker to it.
func assertLabelFallback(t *testing.T, f inlineFixture) {
	t.Helper()
	var label *tg.MessagesSendMessageRequest
	for _, request := range f.user.requests {
		if found, ok := request.(*tg.MessagesSendMessageRequest); ok {
			label = found
		}
	}
	if label == nil || label.Message != "🗣️: Alice\n(Sticker)" {
		t.Fatalf("label message = %#v, want the bold label", label)
	}
	sticker := sentMedia(t, f.user.requests)
	if reply, ok := sticker.ReplyTo.(*tg.InputReplyToMessage); !ok || reply.ReplyToMsgID != f.user.sent-1 {
		t.Fatalf("sticker reply-to = %#v, want the label message", sticker.ReplyTo)
	}
	if len(f.stickers.pending) != 0 {
		t.Fatalf("pending = %d, want the failed nonce dropped", len(f.stickers.pending))
	}
}

func TestUserSessionStickerFallsBackWhenHelperFails(t *testing.T) {
	t.Parallel()
	f := newInlineFixture(&helperInvoker{failUpload: true})
	f.sendLabelled(t)
	assertLabelFallback(t, f)
}

func TestUserSessionStickerFallsBackOnTimeout(t *testing.T) {
	t.Parallel()
	f := newInlineFixture(&helperInvoker{blockUpload: true})
	// The helper waits for the deadline rather than answering, so the budget
	// is what ends the inline route.
	f.stickers.budget = 50 * time.Millisecond
	f.sendLabelled(t)
	assertLabelFallback(t, f)
}

// requestTypes names each request, for failure messages.
func requestTypes(requests []bin.Encoder) string {
	names := make([]string, 0, len(requests))
	for _, request := range requests {
		names = append(names, fmt.Sprintf("%T", request))
	}
	return strings.Join(names, ", ")
}
