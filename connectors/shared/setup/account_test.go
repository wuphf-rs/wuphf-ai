package setup

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"github.com/akshettrj/wuphf/connectors/shared/sessionlock"
	"github.com/propheci/botconf/go/tui"
)

// fakeScreen is a setup screen's session: what an action said, and the
// answers it is given.
type fakeScreen struct {
	said, qrs, progress []string
	answers             []string
	stopped             atomic.Bool
}

func (s *fakeScreen) Say(text string)      { s.said = append(s.said, text) }
func (s *fakeScreen) QR(data string)       { s.qrs = append(s.qrs, data) }
func (s *fakeScreen) Progress(text string) { s.progress = append(s.progress, text) }
func (s *fakeScreen) Cancelled() bool      { return s.stopped.Load() }

func (s *fakeScreen) Ask(string) (string, bool) {
	if len(s.answers) == 0 || s.stopped.Load() {
		return "", false
	}
	answer := s.answers[0]
	s.answers = s.answers[1:]
	return answer, true
}

func (s *fakeScreen) AskSecret(question string) (string, bool) { return s.Ask(question) }

func fakeAccount(session string, closed *bool) OpenAccount {
	return func(tui.Candidate) (*Account, error) {
		return &Account{
			ConnectorID: "whatsapp-main",
			Platform:    "whatsapp",
			Session:     session,
			Link: func(_ context.Context, screen tui.Session) (string, error) {
				screen.QR("code-1")
				if _, ok := screen.Ask("Code:"); !ok {
					return "", errors.New("no code")
				}
				return "Linked as +1", nil
			},
			Chats: func(context.Context, tui.Session) ([]chatdir.Chat, error) {
				return []chatdir.Chat{{Chat: chatdir.ChatRef("whatsapp-main", "1@g.us"), Title: "Family", Kind: chatdir.Group}}, nil
			},
			Close: func() { *closed = true },
		}, nil
	}
}

func TestLinkingAnAccountSavesItsChats(t *testing.T) {
	dir, session := t.TempDir(), filepath.Join(t.TempDir(), "session.db")
	var closed bool
	link, refresh := AccountActions(dir, fakeAccount(session, &closed))[0], AccountActions(dir, fakeAccount(session, &closed))[1]
	screen := &fakeScreen{answers: []string{"123"}}
	notice, err := link.Run(screen, tui.Candidate{})
	if err != nil || !closed {
		t.Fatalf("link: %v (closed %v)", err, closed)
	}
	if want := "Linked as +1; 1 chats saved to " + chatdir.Path(dir, "whatsapp-main"); notice != want {
		t.Fatalf("notice %q", notice)
	}
	if len(screen.qrs) != 1 || len(screen.progress) == 0 {
		t.Fatalf("screen %+v", screen)
	}
	read, err := chatdir.Read(dir, "whatsapp-main")
	if err != nil || read == nil || read.Chats[0].Title != "Family" || read.Platform != "whatsapp" {
		t.Fatalf("read %+v %v", read, err)
	}
	if notice, err := refresh.Run(&fakeScreen{}, tui.Candidate{}); err != nil || !strings.HasPrefix(notice, "1 chats saved") {
		t.Fatalf("refresh: %q %v", notice, err)
	}
}

func TestAnActionRefusesASessionInUse(t *testing.T) {
	session := filepath.Join(t.TempDir(), "session.db")
	held, err := sessionlock.Acquire(session)
	if err != nil {
		t.Fatal(err)
	}
	defer held.Release()
	var closed bool
	_, err = AccountActions(t.TempDir(), fakeAccount(session, &closed))[0].Run(&fakeScreen{}, tui.Candidate{})
	if err == nil || !strings.Contains(err.Error(), "wuphf-whatsapp is running on "+session) {
		t.Fatalf("err %v", err)
	}
}

func TestAStoppedActionSaysSo(t *testing.T) {
	screen := &fakeScreen{}
	screen.stopped.Store(true)
	var closed bool
	_, err := AccountActions(t.TempDir(), fakeAccount(filepath.Join(t.TempDir(), "s"), &closed))[0].Run(screen, tui.Candidate{})
	if err == nil || err.Error() != "Stopped." {
		t.Fatalf("err %v", err)
	}
}

func TestABotLinksWithoutChats(t *testing.T) {
	dir := t.TempDir()
	open := func(tui.Candidate) (*Account, error) {
		return &Account{
			ConnectorID: "telegram-bot", Platform: "telegram", Session: filepath.Join(dir, "bot"),
			Link: func(context.Context, tui.Session) (string, error) { return "The bot token works", nil },
		}, nil
	}
	actions := AccountActions(dir, open)
	if notice, err := actions[0].Run(&fakeScreen{}, tui.Candidate{}); err != nil || notice != "The bot token works" {
		t.Fatalf("link: %q %v", notice, err)
	}
	if _, err := actions[1].Run(&fakeScreen{}, tui.Candidate{}); err == nil {
		t.Fatal("a bot's chats refreshed")
	}
}
