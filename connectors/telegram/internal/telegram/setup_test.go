package telegramconnector

import (
	"context"
	"reflect"
	"testing"

	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	"github.com/gotd/td/telegram/message/peer"
	"github.com/gotd/td/tg"
)

// fakeScreen answers setup's questions, recording them.
type fakeScreen struct {
	asked   []string
	secret  []bool
	answers []string
}

func (s *fakeScreen) Say(string)      {}
func (s *fakeScreen) QR(string)       {}
func (s *fakeScreen) Progress(string) {}
func (s *fakeScreen) Cancelled() bool { return false }

func (s *fakeScreen) Ask(question string) (string, bool) { return s.answer(question, false) }

func (s *fakeScreen) AskSecret(question string) (string, bool) { return s.answer(question, true) }

func (s *fakeScreen) answer(question string, secret bool) (string, bool) {
	s.asked, s.secret = append(s.asked, question), append(s.secret, secret)
	if len(s.answers) == 0 {
		return "", false
	}
	answer := s.answers[0]
	s.answers = s.answers[1:]
	return answer, true
}

func TestDialogsAreTheDirectorysChats(t *testing.T) {
	entities := peer.NewEntities(
		map[int64]*tg.User{
			1: {ID: 1, FirstName: "Alice", LastName: "B", Username: "alice"},
			2: {ID: 2, Self: true},
		},
		map[int64]*tg.Chat{3: {ID: 3, Title: "Family"}},
		map[int64]*tg.Channel{
			4: {ID: 4, Title: "News", Broadcast: true},
			5: {ID: 5, Title: "Club", Megagroup: true},
		},
	)
	var got []chatdir.Chat
	for _, dialog := range []tg.DialogClass{
		&tg.Dialog{Peer: &tg.PeerUser{UserID: 1}},
		&tg.Dialog{Peer: &tg.PeerUser{UserID: 2}},
		&tg.Dialog{Peer: &tg.PeerChat{ChatID: 3}},
		&tg.Dialog{Peer: &tg.PeerChannel{ChannelID: 4}},
		&tg.Dialog{Peer: &tg.PeerChannel{ChannelID: 5}},
		// Unknown to the entities: the id stands for the title.
		&tg.Dialog{Peer: &tg.PeerChannel{ChannelID: 6}},
		&tg.DialogFolder{},
	} {
		if chat, ok := dialogChat("telegram-main", dialog, entities); ok {
			got = append(got, chat)
		}
	}
	want := []chatdir.Chat{
		{Chat: "telegram-main:telegram:user:1", Title: "Alice B (@alice)", Kind: chatdir.Direct},
		{Chat: "telegram-main:telegram:user:2", Title: "Saved Messages", Kind: chatdir.Direct},
		{Chat: "telegram-main:telegram:chat:3", Title: "Family", Kind: chatdir.Group},
		{Chat: "telegram-main:telegram:channel:4", Title: "News", Kind: chatdir.Channel},
		{Chat: "telegram-main:telegram:channel:5", Title: "Club", Kind: chatdir.Group},
		{Chat: "telegram-main:telegram:channel:6", Title: "telegram:channel:6", Kind: chatdir.Group},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("chats %+v", got)
	}
}

func TestTheLoginIsAnsweredOnTheScreen(t *testing.T) {
	ctx := context.Background()
	screen := &fakeScreen{answers: []string{" +15550001 ", "12345", "secret"}}
	login := setupAuthenticator{screen: screen}
	phone, _ := login.Phone(ctx)
	code, _ := login.Code(ctx, nil)
	password, _ := login.Password(ctx)
	if phone != "+15550001" || code != "12345" || password != "secret" {
		t.Fatalf("answers %q %q %q", phone, code, password)
	}
	if !reflect.DeepEqual(screen.secret, []bool{false, false, true}) {
		t.Fatalf("the password is masked: %v", screen.secret)
	}
	// What the environment sets is not asked; a stopped question fails.
	configured := setupAuthenticator{screen: &fakeScreen{}, phone: "+1", password: "pw"}
	if phone, _ := configured.Phone(ctx); phone != "+1" {
		t.Fatalf("phone %q", phone)
	}
	if _, err := configured.Code(ctx, nil); err == nil {
		t.Fatal("a stopped question answered")
	}
}

func TestABotAccountListsNoChats(t *testing.T) {
	cfg := config.File{ConnectorID: "telegram-bot", SessionFile: "bot.session.json"}
	if bot := SetupAccount(cfg, config.Credentials{BotToken: "1:x"}); bot.Chats != nil || bot.Session != "bot.session.json" {
		t.Fatalf("bot %+v", bot)
	}
	if user := SetupAccount(cfg, config.Credentials{Phone: "+1"}); user.Chats == nil {
		t.Fatal("a user lists no chats")
	}
}
