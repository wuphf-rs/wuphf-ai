package instagramconnector

import (
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
