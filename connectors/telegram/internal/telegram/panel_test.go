package telegramconnector

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/bin"
	gotd "github.com/gotd/td/telegram"
	"github.com/gotd/td/tg"
	render "github.com/propheci/botconf/go/render/telegram"
	"github.com/propheci/botconf/go/settings/settingstest"
	"github.com/propheci/botconf/go/settingspb"
	"github.com/rs/zerolog"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

const (
	// testSelf is the session's own Telegram user id.
	testSelf = 42
	// testOwner is someone else writing to the session.
	testOwner = 88
)

// panelAPI is a Telegram API that answers every call with a plausible
// success and records the requests, so a test can see what went on the wire.
type panelAPI struct {
	mu       sync.Mutex
	requests []bin.Encoder
	sent     int
	// before, when set, runs first and can fail the request.
	before func(bin.Encoder) error
}

func (a *panelAPI) Invoke(_ context.Context, input bin.Encoder, output bin.Decoder) error {
	a.mu.Lock()
	a.requests = append(a.requests, input)
	before := a.before
	a.mu.Unlock()
	if before != nil {
		if err := before(input); err != nil {
			return err
		}
	}
	var response bin.Encoder
	switch input.(type) {
	case *tg.MessagesSendMessageRequest, *tg.MessagesSendInlineBotResultRequest:
		a.mu.Lock()
		a.sent++
		response = &tg.UpdateShortSentMessage{ID: 100 + a.sent}
		a.mu.Unlock()
	case *tg.MessagesEditMessageRequest:
		response = &tg.Updates{}
	case *tg.MessagesDeleteMessagesRequest, *tg.ChannelsDeleteMessagesRequest:
		response = &tg.MessagesAffectedMessages{}
	case *tg.MessagesGetInlineBotResultsRequest:
		response = &tg.MessagesBotResults{
			QueryID: 77,
			Results: []tg.BotInlineResultClass{&tg.BotInlineResult{
				ID:          "panel",
				Type:        "article",
				SendMessage: &tg.BotInlineMessageText{Message: "panel"},
			}},
		}
	default:
		response = &tg.BoolTrue{}
	}
	var buffer bin.Buffer
	if err := buffer.Encode(response); err != nil {
		return err
	}
	return output.Decode(&buffer)
}

func (a *panelAPI) Close() error { return nil }

func (a *panelAPI) recorded() []bin.Encoder {
	a.mu.Lock()
	defer a.mu.Unlock()
	return append([]bin.Encoder(nil), a.requests...)
}

// requestsOf returns the recorded requests of type T, in order.
func requestsOf[T bin.Encoder](a *panelAPI) []T {
	var found []T
	for _, request := range a.recorded() {
		if typed, ok := request.(T); ok {
			found = append(found, typed)
		}
	}
	return found
}

// helperBot is a fake helper bot: its own API, and the API it reaches other
// data centers with.
type helperBot struct {
	api *panelAPI
	dc  *panelAPI
}

func (h helperBot) API() *tg.Client { return tg.NewClient(h.api) }

func (h helperBot) DC(context.Context, int, int64) (gotd.CloseInvoker, error) { return h.dc, nil }

// panelFixture is a normalizer with a started panel, a scripted settings
// server, and the session's recorded API.
type panelFixture struct {
	normalizer Normalizer
	panel      *Panel
	server     *settingstest.Server
	api        *panelAPI
	inputs     chan *wuphfpb.ClientFrame
}

func newPanelFixture(t *testing.T, server *settingstest.Server) panelFixture {
	t.Helper()
	peers := NewPeerCache()
	peers.UseBotAddressing()
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	panel := NewPanel("wuphf", peers, zerolog.Nop())
	return panelFixture{
		normalizer: NewNormalizer(peers, inputs, MediaCodec{}).WithPanel(panel),
		panel:      panel,
		server:     server,
		api:        &panelAPI{},
		inputs:     inputs,
	}
}

// botFixture is a bot session's panel.
func botFixture(t *testing.T, server *settingstest.Server) panelFixture {
	f := newPanelFixture(t, server)
	client := settingstest.Start(t, server)
	f.panel.startBot(client, tg.NewClient(f.api), panelSession{self: testSelf, names: []string{"MyBot"}})
	return f
}

// userFixture is a user session's panel, through helper when it is set.
func userFixture(t *testing.T, server *settingstest.Server, helper *helperBot) panelFixture {
	f := newPanelFixture(t, server)
	client := settingstest.Start(t, server)
	api := tg.NewClient(f.api)
	var inline *render.InlinePanel
	if helper == nil {
		inline = render.NewInlinePanel(client, api, nil, nil, testSelf)
	} else {
		inline = render.NewInlinePanel(client, api, *helper, &tg.InputUser{UserID: 7, AccessHash: 9}, testSelf)
	}
	f.panel.startUser(client, api, inline, panelSession{self: testSelf})
	return f
}

// groupMessage is a message from user in a channel.
func groupMessage(id int, user int64, text string) *tg.Message {
	message := &tg.Message{ID: id, PeerID: &tg.PeerChannel{ChannelID: 99}, Message: text, Date: 7}
	message.SetFromID(&tg.PeerUser{UserID: user})
	return message
}

// ownPrivateMessage is the session's own message in a private chat, which
// carries no sender.
func ownPrivateMessage(id int, text string) *tg.Message {
	return &tg.Message{ID: id, Out: true, PeerID: &tg.PeerUser{UserID: 5}, Message: text, Date: 7}
}

func (f panelFixture) normalize(t *testing.T, message *tg.Message) bool {
	t.Helper()
	_, ok, err := f.normalizer.normalize(context.Background(), tg.Entities{}, message)
	if err != nil {
		t.Fatalf("normalize() error = %v", err)
	}
	return ok
}

// postPanel makes every command post a panel with one button.
func postPanel(*settingspb.ActRequest) (*settingspb.ActResponse, error) {
	return &settingspb.ActResponse{Post: &settingspb.Screen{
		Text: &settingspb.Text{Spans: []*settingspb.Span{{Text: "Settings"}}},
		Rows: []*settingspb.Row{{Buttons: []*settingspb.Button{{Label: "Rules", Token: "s:rules"}}}},
	}}, nil
}

// commands are the text commands the server received.
func commands(server *settingstest.Server) []string {
	var texts []string
	for _, request := range server.Acts() {
		if command, ok := request.GetAct().(*settingspb.ActRequest_Command); ok {
			texts = append(texts, command.Command)
		}
	}
	return texts
}

func TestCommandIsTakenNotMirrored(t *testing.T) {
	t.Parallel()
	f := botFixture(t, &settingstest.Server{ActFunc: postPanel})

	if f.normalize(t, groupMessage(10, testOwner, "/wuphf")) {
		t.Fatal("the panel command was mirrored")
	}
	if got := commands(f.server); len(got) != 1 || got[0] != "" {
		t.Fatalf("commands sent to the core = %q, want one empty command (open the panel)", got)
	}
	if acts := f.server.Acts(); acts[0].GetActor().GetId() != "88" {
		t.Fatalf("actor = %v, want the sender", acts[0].GetActor())
	}
	if len(f.inputs) != 0 {
		t.Fatal("the panel command reached the core as a message")
	}
}

func TestOwnCommandIsTakenBeforeOutDrop(t *testing.T) {
	t.Parallel()
	f := userFixture(t, &settingstest.Server{ActFunc: postPanel}, nil)

	if f.normalize(t, ownPrivateMessage(10, "/wuphf")) {
		t.Fatal("the panel command was mirrored")
	}
	renders := f.server.Renders()
	if len(renders) != 1 || renders[0].GetActor().GetId() != "42" {
		t.Fatalf("renders = %v, want one acting for the session's own account", renders)
	}
	if got := commands(f.server); len(got) != 1 {
		t.Fatalf("commands sent to the core = %q, want one", got)
	}
}

func TestOtherOutMessagesStillDropped(t *testing.T) {
	t.Parallel()
	f := userFixture(t, &settingstest.Server{ActFunc: postPanel}, nil)

	if f.normalize(t, ownPrivateMessage(10, "hello")) {
		t.Fatal("an outgoing message was mirrored")
	}
	if len(f.server.Renders()) != 0 || len(f.server.Acts()) != 0 {
		t.Fatal("an ordinary outgoing message reached the settings service")
	}
	if f.normalizer.taken.has("telegram:user:5", 10) {
		t.Fatal("an ordinary outgoing message was taken by the panel")
	}
}

func TestCommandWithBotSuffixIsTaken(t *testing.T) {
	t.Parallel()
	f := botFixture(t, &settingstest.Server{ActFunc: postPanel})

	if f.normalize(t, groupMessage(10, testOwner, "/wuphf@mybot set x 1")) {
		t.Fatal("the addressed panel command was mirrored")
	}
	if got := commands(f.server); len(got) != 1 || got[0] != "set x 1" {
		t.Fatalf("commands sent to the core = %q, want [set x 1]", got)
	}
}

func TestLookalikeTextIsMirrored(t *testing.T) {
	t.Parallel()
	f := botFixture(t, &settingstest.Server{ActFunc: postPanel})

	for i, text := range []string{"/wuphfx", "please /wuphf", "/Wuphf", "/wuphf@otherbot", "wuphf"} {
		if !f.normalize(t, groupMessage(10+i, testOwner, text)) {
			t.Errorf("%q was not mirrored", text)
		}
	}
	if len(f.server.Renders()) != 0 || len(f.server.Acts()) != 0 {
		t.Fatal("a lookalike reached the settings service")
	}
}

func TestCommandIsTakenWhileThePanelIsNotReady(t *testing.T) {
	t.Parallel()
	f := newPanelFixture(t, &settingstest.Server{})

	if f.normalize(t, groupMessage(10, testOwner, "/wuphf")) {
		t.Fatal("the panel command was mirrored before the session started")
	}
	if !f.normalize(t, groupMessage(11, testOwner, "hello")) {
		t.Fatal("an ordinary message was not mirrored before the session started")
	}
}

func TestEditOfTakenMessageIsNotMirrored(t *testing.T) {
	t.Parallel()
	f := botFixture(t, &settingstest.Server{ActFunc: postPanel})

	command := groupMessage(10, testOwner, "/wuphf")
	f.normalize(t, command)
	if _, ok := f.normalizer.normalizeEdit(command); ok {
		t.Fatal("the edit of a panel command was mirrored")
	}
	if _, ok := f.normalizer.normalizeEdit(groupMessage(11, testOwner, "hello")); !ok {
		t.Fatal("the edit of an ordinary message was not mirrored")
	}
}

func TestBotPanelSendsInlineKeyboard(t *testing.T) {
	t.Parallel()
	f := botFixture(t, &settingstest.Server{ActFunc: postPanel})

	f.normalize(t, groupMessage(10, testOwner, "/wuphf"))
	sends := requestsOf[*tg.MessagesSendMessageRequest](f.api)
	if len(sends) != 1 {
		t.Fatalf("sent %d messages, want the panel", len(sends))
	}
	keyboard, ok := sends[0].ReplyMarkup.(*tg.ReplyInlineMarkup)
	if !ok || len(keyboard.Rows) != 1 {
		t.Fatalf("panel markup = %#v, want an inline keyboard", sends[0].ReplyMarkup)
	}
	button, ok := keyboard.Rows[0].Buttons[0].(*tg.KeyboardButtonCallback)
	if !ok || string(button.Data) != "cfg:s:rules" {
		t.Fatalf("button = %#v, want the token behind the cfg: prefix", keyboard.Rows[0].Buttons[0])
	}
	deletes := requestsOf[*tg.ChannelsDeleteMessagesRequest](f.api)
	if len(deletes) != 1 || deletes[0].ID[0] != 10 {
		t.Fatalf("deletes = %v, want the command deleted", deletes)
	}
}

func TestCommandInATopicAnswersInTheTopic(t *testing.T) {
	t.Parallel()
	f := botFixture(t, &settingstest.Server{ActFunc: postPanel})

	command := topicMessage(30, 12, 0)
	command.Message = "/wuphf"
	f.normalize(t, command)
	sends := requestsOf[*tg.MessagesSendMessageRequest](f.api)
	if len(sends) != 1 {
		t.Fatalf("sent %d messages, want the panel", len(sends))
	}
	reply, ok := sends[0].ReplyTo.(*tg.InputReplyToMessage)
	if !ok || reply.ReplyToMsgID != 12 {
		t.Fatalf("panel reply-to = %#v, want the topic root 12", sends[0].ReplyTo)
	}
}

func TestPressRoutesToActAndEditsPanel(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{ActFunc: func(*settingspb.ActRequest) (*settingspb.ActResponse, error) {
		return &settingspb.ActResponse{Screen: &settingspb.Screen{Text: &settingspb.Text{Spans: []*settingspb.Span{{Text: "Rules"}}}}}, nil
	}}
	f := botFixture(t, server)

	taken, err := f.panel.HandleCallback(context.Background(), &tg.UpdateBotCallbackQuery{
		QueryID: 5, UserID: testOwner, Peer: &tg.PeerChannel{ChannelID: 99}, MsgID: 200, Data: []byte("cfg:s:rules"),
	})
	if err != nil || !taken {
		t.Fatalf("HandleCallback() = %v, %v", taken, err)
	}
	acts := server.Acts()
	if len(acts) != 1 || acts[0].GetPress() != "s:rules" || acts[0].GetActor().GetId() != "88" {
		t.Fatalf("acts = %v, want the press as the presser", acts)
	}
	edits := requestsOf[*tg.MessagesEditMessageRequest](f.api)
	if len(edits) != 1 || edits[0].ID != 200 {
		t.Fatalf("edits = %v, want the panel message redrawn", edits)
	}
	if answers := requestsOf[*tg.MessagesSetBotCallbackAnswerRequest](f.api); len(answers) != 1 {
		t.Fatalf("answered the press %d times, want once", len(answers))
	}
}

func TestOtherCallbacksAreLeftToTheConnector(t *testing.T) {
	t.Parallel()
	f := botFixture(t, &settingstest.Server{})

	taken, err := f.panel.HandleCallback(context.Background(), &tg.UpdateBotCallbackQuery{
		QueryID: 5, UserID: testOwner, Peer: &tg.PeerChannel{ChannelID: 99}, Data: []byte(labelCallbackData),
	})
	if err != nil || taken {
		t.Fatalf("HandleCallback(%q) = %v, %v; the sticker label must stay the connector's", labelCallbackData, taken, err)
	}
}

func TestUnauthorizedPressGetsNotAllowedToast(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{ActFunc: func(*settingspb.ActRequest) (*settingspb.ActResponse, error) {
		return nil, status.Error(codes.PermissionDenied, "not an owner")
	}}
	f := botFixture(t, server)

	taken, _ := f.panel.HandleCallback(context.Background(), &tg.UpdateBotCallbackQuery{
		QueryID: 5, UserID: 1234, Peer: &tg.PeerChannel{ChannelID: 99}, MsgID: 200, Data: []byte("cfg:s:rules"),
	})
	if !taken {
		t.Fatal("the press was not taken")
	}
	answers := requestsOf[*tg.MessagesSetBotCallbackAnswerRequest](f.api)
	if len(answers) != 1 || !strings.Contains(answers[0].Message, "may not") {
		t.Fatalf("answers = %v, want one refusal toast", answers)
	}
	if len(requestsOf[*tg.MessagesEditMessageRequest](f.api)) != 0 {
		t.Fatal("a refused press redrew the panel")
	}
}

func TestUnauthorizedCommandGetsNoReply(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{
		RenderFunc: func(*settingspb.RenderRequest) (*settingspb.Screen, error) {
			return nil, status.Error(codes.PermissionDenied, "not an owner")
		},
		ActFunc: postPanel,
	}
	f := botFixture(t, server)

	if f.normalize(t, groupMessage(10, 1234, "/wuphf")) {
		t.Fatal("a stranger's panel command was mirrored")
	}
	if len(f.api.recorded()) != 0 {
		t.Fatalf("a stranger's command got Telegram requests: %v", f.api.recorded())
	}
	if len(server.Acts()) != 0 {
		t.Fatal("a stranger's command reached Act")
	}
}

func TestStrangerCannotDriveAUserSessionsPanel(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{ActFunc: postPanel}
	f := userFixture(t, server, nil)

	if f.normalize(t, groupMessage(10, testOwner, "/wuphf set x 1")) {
		t.Fatal("a stranger's panel command was mirrored")
	}
	if len(server.Renders()) != 0 || len(server.Acts()) != 0 || len(f.api.recorded()) != 0 {
		t.Fatal("a stranger's command reached the user session's panel")
	}
}

func TestTextSubcommandGoesToAct(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{ActFunc: func(*settingspb.ActRequest) (*settingspb.ActResponse, error) {
		return &settingspb.ActResponse{Reply: &settingspb.Text{Spans: []*settingspb.Span{{Text: "set"}}}}, nil
	}}
	f := botFixture(t, server)

	f.normalize(t, groupMessage(10, testOwner, "/wuphf set rules.x {\"kind\": \"many-to-one\"}"))
	if got := commands(server); len(got) != 1 || got[0] != `set rules.x {"kind": "many-to-one"}` {
		t.Fatalf("commands sent to the core = %q", got)
	}
	if sends := requestsOf[*tg.MessagesSendMessageRequest](f.api); len(sends) != 1 || sends[0].Message != "set" {
		t.Fatalf("sends = %v, want the command's reply", sends)
	}
}

func TestPromptAnswerGoesToActAndIsDeleted(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{ActFunc: func(request *settingspb.ActRequest) (*settingspb.ActResponse, error) {
		if request.GetPress() != "" {
			return &settingspb.ActResponse{Ask: &settingspb.Ask{
				Question: "q1",
				Text:     &settingspb.Text{Spans: []*settingspb.Span{{Text: "New value?"}}},
			}}, nil
		}
		return &settingspb.ActResponse{Screen: &settingspb.Screen{Text: &settingspb.Text{Spans: []*settingspb.Span{{Text: "Done"}}}}}, nil
	}}
	f := botFixture(t, server)

	if _, err := f.panel.HandleCallback(context.Background(), &tg.UpdateBotCallbackQuery{
		QueryID: 5, UserID: testOwner, Peer: &tg.PeerChannel{ChannelID: 99}, MsgID: 200, Data: []byte("cfg:a:1"),
	}); err != nil {
		t.Fatalf("HandleCallback() error = %v", err)
	}
	if f.normalize(t, groupMessage(11, testOwner, "1000")) {
		t.Fatal("the answer to a question was mirrored")
	}
	acts := server.Acts()
	answer := acts[len(acts)-1].GetAnswer()
	if answer.GetQuestion() != "q1" || answer.GetText() != "1000" {
		t.Fatalf("answer = %v, want the typed text for q1", answer)
	}
	var deleted []int
	for _, request := range requestsOf[*tg.ChannelsDeleteMessagesRequest](f.api) {
		deleted = append(deleted, request.ID...)
	}
	if !containsID(deleted, 11) {
		t.Fatalf("deleted %v, want the answer (11) deleted", deleted)
	}
	if !f.normalize(t, groupMessage(12, testOwner, "after")) {
		t.Fatal("a message after the answer was not mirrored")
	}
}

func containsID(ids []int, id int) bool {
	for _, candidate := range ids {
		if candidate == id {
			return true
		}
	}
	return false
}

func TestUserSessionSendsPanelThroughInlineResult(t *testing.T) {
	t.Parallel()
	helper := &helperBot{api: &panelAPI{}, dc: &panelAPI{}}
	f := userFixture(t, &settingstest.Server{ActFunc: postPanel}, helper)
	// The helper bot answers the account's inline query while the account
	// waits for the results.
	f.api.before = func(request bin.Encoder) error {
		if query, ok := request.(*tg.MessagesGetInlineBotResultsRequest); ok {
			_, err := f.panel.HandleInlineQuery(context.Background(), &tg.UpdateBotInlineQuery{QueryID: 3, UserID: testSelf, Query: query.Query})
			return err
		}
		return nil
	}

	if f.normalize(t, ownPrivateMessage(10, "/wuphf")) {
		t.Fatal("the panel command was mirrored")
	}
	var order []string
	for _, request := range f.api.recorded() {
		switch request.(type) {
		case *tg.MessagesGetInlineBotResultsRequest:
			order = append(order, "get")
		case *tg.MessagesSendInlineBotResultRequest:
			order = append(order, "send")
		case *tg.MessagesSendMessageRequest:
			order = append(order, "text")
		}
	}
	if strings.Join(order, ",") != "get,send" {
		t.Fatalf("user session requests = %v, want get then send of the inline result", order)
	}
	query := requestsOf[*tg.MessagesGetInlineBotResultsRequest](f.api)[0]
	if len(query.Query) != 22 {
		t.Fatalf("inline query = %q, want a 22-character nonce", query.Query)
	}
	answers := requestsOf[*tg.MessagesSetInlineBotResultsRequest](helper.api)
	if len(answers) != 1 || len(answers[0].Results) != 1 || !answers[0].Private {
		t.Fatalf("helper answers = %v, want one private result", answers)
	}
}

func TestHelperIgnoresQueriesFromOtherUsersOrUnknownNonces(t *testing.T) {
	t.Parallel()
	helper := &helperBot{api: &panelAPI{}, dc: &panelAPI{}}
	f := userFixture(t, &settingstest.Server{ActFunc: postPanel}, helper)

	taken, err := f.panel.HandleInlineQuery(context.Background(), &tg.UpdateBotInlineQuery{QueryID: 1, UserID: testSelf, Query: "made-up"})
	if err != nil || taken {
		t.Fatalf("HandleInlineQuery(unknown nonce) = %v, %v; want it left to the connector", taken, err)
	}

	// A stranger racing for a pending nonce gets nothing, and the nonce stays
	// usable by the account.
	f.api.before = func(request bin.Encoder) error {
		query, ok := request.(*tg.MessagesGetInlineBotResultsRequest)
		if !ok {
			return nil
		}
		if _, err := f.panel.HandleInlineQuery(context.Background(), &tg.UpdateBotInlineQuery{QueryID: 2, UserID: 1234, Query: query.Query}); err != nil {
			return err
		}
		_, err := f.panel.HandleInlineQuery(context.Background(), &tg.UpdateBotInlineQuery{QueryID: 3, UserID: testSelf, Query: query.Query})
		return err
	}
	f.normalize(t, ownPrivateMessage(10, "/wuphf"))
	answers := requestsOf[*tg.MessagesSetInlineBotResultsRequest](helper.api)
	if len(answers) != 2 {
		t.Fatalf("helper answered %d queries, want 2", len(answers))
	}
	if answers[0].QueryID != 2 || len(answers[0].Results) != 0 {
		t.Fatalf("the stranger got %v, want no results", answers[0])
	}
	if answers[1].QueryID != 3 || len(answers[1].Results) != 1 {
		t.Fatalf("the account got %v, want the panel", answers[1])
	}
}

func TestInlinePressEditsInlineMessage(t *testing.T) {
	t.Parallel()
	helper := &helperBot{api: &panelAPI{}, dc: &panelAPI{}}
	server := &settingstest.Server{ActFunc: func(*settingspb.ActRequest) (*settingspb.ActResponse, error) {
		return &settingspb.ActResponse{Screen: &settingspb.Screen{Text: &settingspb.Text{Spans: []*settingspb.Span{{Text: "Rules"}}}}}, nil
	}}
	f := userFixture(t, server, helper)

	id := &tg.InputBotInlineMessageID{DCID: 2, ID: 5, AccessHash: 6}
	taken, err := f.panel.HandleInlineCallback(context.Background(), &tg.UpdateInlineBotCallbackQuery{
		QueryID: 9, UserID: testSelf, MsgID: id, Data: []byte("cfg:s:rules"),
	})
	if err != nil || !taken {
		t.Fatalf("HandleInlineCallback() = %v, %v", taken, err)
	}
	edits := requestsOf[*tg.MessagesEditInlineBotMessageRequest](helper.dc)
	if len(edits) != 1 || edits[0].Message != "Rules" {
		t.Fatalf("inline edits = %v, want the panel redrawn on its data center", edits)
	}
	if acts := server.Acts(); len(acts) != 1 || acts[0].GetActor().GetId() != "42" {
		t.Fatalf("acts = %v, want the press as the presser", acts)
	}
}

func TestInlineFailureFallsBackToText(t *testing.T) {
	t.Parallel()
	helper := &helperBot{api: &panelAPI{}, dc: &panelAPI{}}
	f := userFixture(t, &settingstest.Server{ActFunc: postPanel}, helper)
	f.api.before = func(request bin.Encoder) error {
		if _, ok := request.(*tg.MessagesGetInlineBotResultsRequest); ok {
			return errors.New("BOT_INLINE_DISABLED")
		}
		return nil
	}

	f.normalize(t, ownPrivateMessage(10, "/wuphf"))
	sends := requestsOf[*tg.MessagesSendMessageRequest](f.api)
	if len(sends) != 1 || !strings.Contains(sends[0].Message, "1. Rules") {
		t.Fatalf("sends = %v, want the panel as text with numbered options", sends)
	}
	if len(requestsOf[*tg.MessagesSendInlineBotResultRequest](f.api)) != 0 {
		t.Fatal("a failed inline route still sent an inline result")
	}
	// Replying with an option's number presses it.
	if f.normalize(t, ownPrivateMessage(11, "1")) {
		t.Fatal("the option number was mirrored")
	}
	acts := f.server.Acts()
	if press := acts[len(acts)-1].GetPress(); press != "s:rules" {
		t.Fatalf("press = %q, want the first option's token", press)
	}
}

func TestMatchCommand(t *testing.T) {
	t.Parallel()
	names := map[string]bool{"mybot": true}
	for _, test := range []struct {
		text, args string
		ok         bool
	}{
		{"/wuphf", "", true},
		{"/wuphf set x 1", "set x 1", true},
		{"/wuphf@MyBot help", "help", true},
		{"/wuphf\nset x 1", "set x 1", true},
		{"/wuphf@other", "", false},
		{"/wuphfx", "", false},
		{" /wuphf", "", false},
		{"/WUPHF", "", false},
	} {
		args, ok := matchCommand(test.text, "wuphf", names)
		if ok != test.ok || args != test.args {
			t.Errorf("matchCommand(%q) = %q, %v; want %q, %v", test.text, args, ok, test.args, test.ok)
		}
	}
}

// Compile-time check that the fake helper satisfies the renderer's interface.
var _ render.HelperBot = helperBot{}
