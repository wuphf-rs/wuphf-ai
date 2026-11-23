package whatsappconnector

import (
	"context"
	"errors"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow/types"
)

// fakeScreen records what setup's pairing shows.
type fakeScreen struct {
	said, qrs, progress []string
}

func (s *fakeScreen) Say(text string)                 { s.said = append(s.said, text) }
func (s *fakeScreen) QR(data string)                  { s.qrs = append(s.qrs, data) }
func (s *fakeScreen) Progress(text string)            { s.progress = append(s.progress, text) }
func (s *fakeScreen) Ask(string) (string, bool)       { return "", false }
func (s *fakeScreen) AskSecret(string) (string, bool) { return "", false }
func (s *fakeScreen) Cancelled() bool                 { return false }

func TestTheDirectoryHoldsGroupsSavedContactsAndChannels(t *testing.T) {
	group := types.NewJID("120363000000000001", types.GroupServer)
	saved := types.NewJID("15550000001", types.DefaultUserServer)
	business := types.NewJID("15550000002", types.DefaultUserServer)
	seenInGroup := types.NewJID("15550000003", types.DefaultUserServer)
	channel := types.NewJID("120363000000000009", types.NewsletterServer)
	lid := types.NewJID("99999", types.HiddenUserServer)
	newsletter := &types.NewsletterMetadata{ID: channel}
	newsletter.ThreadMeta.Name.Text = "Club news"
	chats := directoryChats("whatsapp-main",
		[]*types.GroupInfo{{JID: group, GroupName: types.GroupName{Name: "Family"}}},
		map[types.JID]types.ContactInfo{
			saved:       {FullName: "Alice"},
			business:    {BusinessName: "Bakery"},
			seenInGroup: {PushName: "Someone"},
		},
		[]*types.NewsletterMetadata{newsletter},
		// The connector reports Alice by her LID.
		func(jid types.JID) types.JID {
			if jid == saved {
				return lid
			}
			return jid
		})
	want := []chatdir.Chat{
		{Chat: "whatsapp-main:99999@lid", Title: "Alice", Kind: chatdir.Direct},
		{Chat: "whatsapp-main:15550000002@s.whatsapp.net", Title: "Bakery (Business)", Kind: chatdir.Direct},
		{Chat: "whatsapp-main:120363000000000009@newsletter", Title: "Club news", Kind: chatdir.Channel},
		{Chat: "whatsapp-main:120363000000000001@g.us", Title: "Family", Kind: chatdir.Group},
	}
	if !reflect.DeepEqual(chats, want) {
		t.Fatalf("chats %+v", chats)
	}
}

func TestSetupShowsTheConfiguredCodes(t *testing.T) {
	ctx := context.Background()
	qr := &fakeScreen{}
	codes := &setupCodes{screen: qr, method: config.LoginMethodQR}
	if _, ok := codes.PairCodeAsk(); ok {
		t.Fatal("a QR link asks for a pair code")
	}
	codes.QR(ctx, "2@first", 60*time.Second)
	codes.QR(ctx, "2@second", 20*time.Second)
	if !reflect.DeepEqual(qr.qrs, []string{"2@first", "2@second"}) || !strings.Contains(qr.progress[1], "valid for 20 s") {
		t.Fatalf("screen %+v", qr)
	}

	paired := &fakeScreen{}
	codes = &setupCodes{screen: paired, method: config.LoginMethodPairCode}
	ask, ok := codes.PairCodeAsk()
	if !ok || !ask.promptPhone {
		t.Fatalf("ask %+v %v", ask, ok)
	}
	codes.QR(ctx, "2@ignored", time.Minute)
	codes.PairCode(ctx, "ABCD-EFGH", ask)
	codes.PairCodeFailed(errors.New("rate limited"))
	if len(paired.qrs) != 0 || !strings.HasPrefix(paired.said[0], "Pair code ABCD-EFGH") ||
		!strings.Contains(paired.said[1], "rate limited") {
		t.Fatalf("screen %+v", paired)
	}
}

func TestSetupLocksTheSessionFile(t *testing.T) {
	account := SetupAccount(config.File{ConnectorID: "whatsapp-main", SessionDatabase: "file:wa.db?_foreign_keys=on"}, zerolog.Nop())
	if account.Session != "wa.db" || account.Platform != "whatsapp" || account.Chats == nil {
		t.Fatalf("account %+v", account)
	}
}
