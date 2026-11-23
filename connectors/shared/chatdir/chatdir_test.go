package chatdir

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

// The fixture the core (wuphf-model's chat_directory) decodes too.
func fixture(t *testing.T) []byte {
	t.Helper()
	text, err := os.ReadFile("testdata/directory.json")
	if err != nil {
		t.Fatal(err)
	}
	return text
}

func TestTheSharedFixtureReadsAndWritesBack(t *testing.T) {
	var directory Directory
	if err := json.Unmarshal(fixture(t), &directory); err != nil {
		t.Fatal(err)
	}
	if directory.ConnectorID != "whatsapp-main" || len(directory.Chats) != 4 || directory.Chats[1].Kind != Direct {
		t.Fatalf("directory %+v", directory)
	}
	if got := ChatRef("whatsapp-main", "odd#id") + "#7"; got != directory.Chats[3].Chat {
		t.Fatalf("ChatRef %q", got)
	}
	dir := t.TempDir()
	path, err := directory.Write(dir)
	if err != nil {
		t.Fatal(err)
	}
	written, _ := os.ReadFile(path)
	if !bytes.Equal(written, fixture(t)) {
		t.Fatalf("written:\n%s", written)
	}
}

func TestAFileIsWrittenWholeAndReadBack(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "nested")
	if got, err := Read(dir, "whatsapp-main"); got != nil || err != nil {
		t.Fatalf("no file: %v %v", got, err)
	}
	directory := New("whatsapp-main", "whatsapp", 1, nil)
	if _, err := directory.Write(dir); err != nil {
		t.Fatal(err)
	}
	got, err := Read(dir, "whatsapp-main")
	if err != nil || !reflect.DeepEqual(*got, directory) {
		t.Fatalf("read %+v %v", got, err)
	}
	if _, err := os.Stat(Path(dir, "whatsapp-main") + ".tmp"); !os.IsNotExist(err) {
		t.Fatal("the temporary file is left")
	}
}

func TestAnotherVersionOrConnectorIsRefused(t *testing.T) {
	dir := t.TempDir()
	path := Path(dir, "whatsapp-main")
	for text, want := range map[string]string{
		`{"version": 2, "whatever": true}`:                                   "is version 2, not 1",
		strings.Replace(string(fixture(t)), `"whatsapp-main"`, `"other"`, 1): "holds the chats of other",
		"{": "is not a chat directory",
	} {
		os.WriteFile(path, []byte(text), 0o644)
		if _, err := Read(dir, "whatsapp-main"); err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("%q: %v", text, err)
		}
	}
}

func TestChatsMatchByTitleOrID(t *testing.T) {
	var directory Directory
	json.Unmarshal(fixture(t), &directory)
	found := func(text string) []string {
		var out []string
		for _, chat := range directory.Chats {
			if chat.Matches(text) {
				out = append(out, chat.Title)
			}
		}
		return out
	}
	if got := found("FOOT"); !reflect.DeepEqual(got, []string{"Football club news"}) {
		t.Fatalf("found %v", got)
	}
	if got := found("9198"); !reflect.DeepEqual(got, []string{"Alice"}) {
		t.Fatalf("found %v", got)
	}
	if len(found("")) != 4 {
		t.Fatal("empty text matches everything")
	}
}

func TestTheDirectoryComesFromTheEnvironment(t *testing.T) {
	env := func(pairs map[string]string) func(string) string {
		return func(name string) string { return pairs[name] }
	}
	for want, pairs := range map[string]map[string]string{
		"/chats":                           {DirectoryEnv: "/chats", "XDG_STATE_HOME": "/state"},
		"/state/wuphf/chats":               {"XDG_STATE_HOME": "/state", "HOME": "/home/u"},
		"/home/u/.local/state/wuphf/chats": {"HOME": "/home/u"},
		"":                                 {},
	} {
		if got := DirFrom(env(pairs)); got != want {
			t.Errorf("%v: %q, want %q", pairs, got, want)
		}
	}
}
