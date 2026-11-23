package instagramconnector

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
)

func TestTheInboxThreadsAreTheDirectorysChats(t *testing.T) {
	threads := NewThreadDirectory(nil, 1)
	threads.RecordUser(&slidetypes.User{InteropMessagingUserFBID: 2, FullName: "Alice"})
	threads.RecordThread(&slidetypes.ThreadInfo{ThreadKey: 20, Users: []*slidetypes.User{
		{InteropMessagingUserFBID: 1}, {InteropMessagingUserFBID: 2, FullName: "Alice"},
	}})
	threads.RecordThread(&slidetypes.ThreadInfo{ThreadKey: 10, ThreadTitle: "Family", IsGroup: true})
	threads.RecordThread(&slidetypes.ThreadInfo{ThreadKey: 30})
	got := directoryChats("instagram-main", threads.Threads())
	want := []chatdir.Chat{
		{Chat: "instagram-main:30", Title: "30", Kind: chatdir.Direct},
		{Chat: "instagram-main:20", Title: "Alice", Kind: chatdir.Direct},
		{Chat: "instagram-main:10", Title: "Family", Kind: chatdir.Group},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("chats %+v", got)
	}
}

func TestCookiesArePastedOrNamed(t *testing.T) {
	const pasted = `{"sessionid": "s", "csrftoken": "c", "ds_user_id": "1", "mid": "m", "ig_did": "d"}`
	jar, err := readCookies(pasted)
	if err != nil || !jar.IsLoggedIn() {
		t.Fatalf("pasted: %v (missing %v)", err, jar.GetMissingCookieNames())
	}
	path := filepath.Join(t.TempDir(), "cookies.json")
	os.WriteFile(path, []byte(pasted), 0o600)
	if jar, err := readCookies(path); err != nil || !jar.IsLoggedIn() {
		t.Fatalf("named: %v", err)
	}
	if _, err := readCookies("{not json"); err == nil || !strings.Contains(err.Error(), "not a JSON map") {
		t.Fatalf("bad JSON: %v", err)
	}
	if _, err := readCookies(filepath.Join(t.TempDir(), "missing.json")); err == nil {
		t.Fatal("a missing file read")
	}
}

// pages answers PaginateMailbox with its pages in turn, by cursor.
type pages struct {
	byCursor map[string]*slidetypes.MailboxPage
	asked    []string
}

func (p *pages) PaginateMailbox(_ context.Context, request *slidetypes.PaginateMailboxRequest) (*slidetypes.PaginateMailboxResponse, error) {
	p.asked = append(p.asked, request.Cursor)
	page, ok := p.byCursor[request.Cursor]
	if !ok {
		return nil, errors.New("no such page")
	}
	return &slidetypes.PaginateMailboxResponse{Mailbox: page}, nil
}

func threadPage(next string, keys ...int64) slidetypes.Edged[slidetypes.Node[slidetypes.WrappedThreadInfo]] {
	var page slidetypes.Edged[slidetypes.Node[slidetypes.WrappedThreadInfo]]
	for _, key := range keys {
		page.Edges = append(page.Edges, slidetypes.Node[slidetypes.WrappedThreadInfo]{
			Node: slidetypes.WrappedThreadInfo{AsIGDirectThread: &slidetypes.ThreadInfo{ThreadKey: key, ThreadTitle: fmt.Sprint("T", key)}},
		})
	}
	page.PageInfo = slidetypes.PageInfo{EndCursor: next, HasNextPage: next != ""}
	return page
}

func TestTheWholeInboxIsListed(t *testing.T) {
	first := &slidetypes.Mailbox{ThreadsByFolder: threadPage("c1", 1, 2)}
	pager := &pages{byCursor: map[string]*slidetypes.MailboxPage{
		"c1": {ThreadsByFolder: threadPage("c2", 3)},
		"c2": {ThreadsByFolder: threadPage("", 4)},
	}}
	threads := NewThreadDirectory(nil, 1)
	threads.RecordMailbox(first)
	if err := recordInbox(context.Background(), &nopScreen{}, pager, 1, first, threads); err != nil {
		t.Fatal(err)
	}
	if got := len(threads.Threads()); got != 4 {
		t.Fatalf("threads %d, asked %v", got, pager.asked)
	}
	// A failed page keeps what came before it.
	broken := &pages{byCursor: map[string]*slidetypes.MailboxPage{}}
	partial := NewThreadDirectory(nil, 1)
	partial.RecordMailbox(first)
	if err := recordInbox(context.Background(), &nopScreen{}, broken, 1, first, partial); err == nil || len(partial.Threads()) != 2 {
		t.Fatalf("broken: %v, %d threads", err, len(partial.Threads()))
	}
}

type nopScreen struct{}

func (nopScreen) Say(string)                      {}
func (nopScreen) QR(string)                       {}
func (nopScreen) Progress(string)                 {}
func (nopScreen) Ask(string) (string, bool)       { return "", false }
func (nopScreen) AskSecret(string) (string, bool) { return "", false }
func (nopScreen) Cancelled() bool                 { return false }
