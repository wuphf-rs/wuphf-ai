package telegramconnector

import (
	"context"
	"testing"

	"github.com/gotd/td/tg"
	"github.com/propheci/botconf/go/settings/settingstest"
	"github.com/propheci/botconf/go/settingspb"
	"github.com/rs/zerolog"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// helperChatsFixture is the helper bot's dispatcher with its private-chat
// panel started, a scripted settings server and the helper's recorded API.
type helperChatsFixture struct {
	dispatcher tg.UpdateDispatcher
	server     *settingstest.Server
	api        *panelAPI
}

func newHelperChatsFixture(t *testing.T, server *settingstest.Server) helperChatsFixture {
	t.Helper()
	api := &panelAPI{}
	helperAPI := tg.NewClient(api)
	chats := NewHelperChats("wuphf", zerolog.Nop())
	chats.start(settingstest.Start(t, server), helperAPI, "PropheciTestBot")
	panel := NewPanel("wuphf", NewPeerCache(), zerolog.Nop())
	return helperChatsFixture{
		dispatcher: newHelperDispatcher(panel, NewInlineStickers(zerolog.Nop()), chats, func() *tg.Client { return helperAPI }),
		server:     server,
		api:        api,
	}
}

// handle feeds the helper one update, with user's entity carrying hash 555.
func (f helperChatsFixture) handle(t *testing.T, user int64, update tg.UpdateClass) {
	t.Helper()
	updates := &tg.Updates{
		Updates: []tg.UpdateClass{update},
		Users:   []tg.UserClass{&tg.User{ID: user, AccessHash: 555}},
	}
	if err := f.dispatcher.Handle(context.Background(), updates); err != nil {
		t.Fatalf("helper update error = %v", err)
	}
}

// privateToHelper is a message user sends the helper bot in a private chat.
func privateToHelper(id int, user int64, text string) *tg.UpdateNewMessage {
	return &tg.UpdateNewMessage{Message: &tg.Message{ID: id, PeerID: &tg.PeerUser{UserID: user}, Message: text, Date: 7}}
}

func TestOwnerOpensThePanelInAPrivateChatWithTheHelper(t *testing.T) {
	t.Parallel()
	f := newHelperChatsFixture(t, &settingstest.Server{ActFunc: postPanel})

	f.handle(t, testOwner, privateToHelper(10, testOwner, "/wuphf"))

	if renders := f.server.Renders(); len(renders) != 1 || renders[0].GetActor().GetId() != "88" {
		t.Fatalf("renders = %v, want one probe acting for the sender", renders)
	}
	if got := commands(f.server); len(got) != 1 || got[0] != "" {
		t.Fatalf("commands sent to the core = %q, want one empty command (open the panel)", got)
	}
	sends := requestsOf[*tg.MessagesSendMessageRequest](f.api)
	if len(sends) != 1 {
		t.Fatalf("the helper sent %d messages, want the panel", len(sends))
	}
	peer, ok := sends[0].Peer.(*tg.InputPeerUser)
	if !ok || peer.UserID != testOwner || peer.AccessHash != 555 {
		t.Fatalf("panel sent to %#v, want the owner's private chat with their access hash", sends[0].Peer)
	}
	if _, ok := sends[0].ReplyMarkup.(*tg.ReplyInlineMarkup); !ok {
		t.Fatalf("panel markup = %T, want an inline keyboard", sends[0].ReplyMarkup)
	}
}

func TestAddressedCommandInAPrivateChatWithTheHelper(t *testing.T) {
	t.Parallel()
	f := newHelperChatsFixture(t, &settingstest.Server{ActFunc: postPanel})

	f.handle(t, testOwner, privateToHelper(10, testOwner, "/wuphf@propheciTestBot set x 1"))
	f.handle(t, testOwner, privateToHelper(11, testOwner, "/wuphf@SomeOtherBot"))

	if got := commands(f.server); len(got) != 1 || got[0] != "set x 1" {
		t.Fatalf("commands sent to the core = %q, want [set x 1] only", got)
	}
}

func TestStrangerGetsNoReplyFromTheHelper(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{
		RenderFunc: func(*settingspb.RenderRequest) (*settingspb.Screen, error) {
			return nil, status.Error(codes.PermissionDenied, "not an owner")
		},
		ActFunc: postPanel,
	}
	f := newHelperChatsFixture(t, server)

	f.handle(t, 1234, privateToHelper(10, 1234, "/wuphf"))

	if len(server.Acts()) != 0 {
		t.Fatal("a stranger's command reached Act")
	}
	if requests := f.api.recorded(); len(requests) != 0 {
		t.Fatalf("a stranger's command got Telegram requests: %s", requestTypes(requests))
	}
}

func TestPressInAPrivateChatWithTheHelperReachesThePanel(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{ActFunc: func(*settingspb.ActRequest) (*settingspb.ActResponse, error) {
		return &settingspb.ActResponse{Screen: &settingspb.Screen{Text: &settingspb.Text{Spans: []*settingspb.Span{{Text: "Rules"}}}}}, nil
	}}
	f := newHelperChatsFixture(t, server)

	f.handle(t, testOwner, &tg.UpdateBotCallbackQuery{
		QueryID: 5, UserID: testOwner, Peer: &tg.PeerUser{UserID: testOwner}, MsgID: 200, Data: []byte("cfg:s:rules"),
	})

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

func TestHelperIgnoresGroupMessages(t *testing.T) {
	t.Parallel()
	f := newHelperChatsFixture(t, &settingstest.Server{ActFunc: postPanel})

	group := &tg.Message{ID: 10, PeerID: &tg.PeerChat{ChatID: 99}, Message: "/wuphf", Date: 7}
	group.SetFromID(&tg.PeerUser{UserID: testOwner})
	f.handle(t, testOwner, &tg.UpdateNewMessage{Message: group})
	f.handle(t, testOwner, &tg.UpdateNewChannelMessage{Message: groupMessage(11, testOwner, "/wuphf")})

	if len(f.server.Renders()) != 0 || len(f.server.Acts()) != 0 {
		t.Fatal("a group message to the helper reached the settings service")
	}
	if requests := f.api.recorded(); len(requests) != 0 {
		t.Fatalf("a group message got Telegram requests: %s", requestTypes(requests))
	}
}

func TestHelperAnswersOtherPressesSilently(t *testing.T) {
	t.Parallel()
	f := newHelperChatsFixture(t, &settingstest.Server{ActFunc: postPanel})

	// A sticker's label button, pressed on an inline sticker message.
	f.handle(t, testOwner, &tg.UpdateInlineBotCallbackQuery{QueryID: 33, UserID: testOwner, Data: []byte(labelCallbackData)})
	// A press in a group the helper is in.
	f.handle(t, testOwner, &tg.UpdateBotCallbackQuery{QueryID: 34, UserID: testOwner, Peer: &tg.PeerChat{ChatID: 99}, Data: []byte("cfg:s:rules")})

	if len(f.server.Acts()) != 0 {
		t.Fatal("a press that is not the private panel's reached Act")
	}
	answers := requestsOf[*tg.MessagesSetBotCallbackAnswerRequest](f.api)
	if len(answers) != 2 {
		t.Fatalf("answered %d presses, want both", len(answers))
	}
	for _, answer := range answers {
		if answer.Message != "" || answer.Alert {
			t.Fatalf("press answer = %#v, want a silent answer", answer)
		}
	}
}

func TestHelperChatsWaitForTheHelperToStart(t *testing.T) {
	t.Parallel()
	server := &settingstest.Server{ActFunc: postPanel}
	chats := NewHelperChats("wuphf", zerolog.Nop())

	taken, err := chats.HandleMessage(context.Background(), tg.Entities{}, privateToHelper(10, testOwner, "/wuphf").Message)
	if err != nil || taken {
		t.Fatalf("HandleMessage() before start = %v, %v; want nothing taken", taken, err)
	}
	if len(server.Renders()) != 0 {
		t.Fatal("a command before start reached the settings service")
	}
}
