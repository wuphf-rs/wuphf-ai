package setup

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/propheci/botconf/go/describe"
	"github.com/propheci/botconf/go/tui"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/chatdir"
)

type sample struct {
	Address  string `toml:"address" doc:"Where to connect." required:"true" example:"127.0.0.1:1"`
	TokenEnv string `toml:"token_env" env:"token" hint:"SAMPLE_TOKEN"`
}

func TestCheckExample(t *testing.T) {
	description := describe.MustOf("Sample", "", sample{})
	path := filepath.Join(t.TempDir(), "config.example.toml")
	if err := os.WriteFile(path, []byte("stale"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := CheckExample(description, path); err == nil || !strings.Contains(err.Error(), WriteExamples) {
		t.Fatalf("a stale example passes: %v", err)
	}
	t.Setenv(WriteExamples, "1")
	if err := CheckExample(description, path); err != nil {
		t.Fatalf("written, it still differs: %v", err)
	}
	t.Setenv(WriteExamples, "")
	if err := CheckExample(description, path); err != nil {
		t.Fatal(err)
	}
}

func TestRequireToken(t *testing.T) {
	t.Setenv("SAMPLE_TOKEN", "")
	candidate := tui.Candidate{Env: map[string]string{}}
	if err := RequireToken(candidate, "token_env", "SAMPLE_TOKEN"); err == nil {
		t.Fatal("an unset token passes")
	}
	candidate.Env["SAMPLE_TOKEN"] = "from the env file"
	if err := RequireToken(candidate, "token_env", "SAMPLE_TOKEN"); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OTHER_TOKEN", "from the process")
	if err := RequireToken(candidate, "token_env", "OTHER_TOKEN"); err != nil {
		t.Fatal(err)
	}
	if !Requested([]string{"setup", "-config", "x"}) || Requested([]string{"-config", "x"}) {
		t.Fatal("Requested")
	}
}

func TestChatsAreSuggestedFromEveryConnectorsFile(t *testing.T) {
	dir := t.TempDir()
	ask := func(suggest tui.SuggestFunc, domain, text string) ([]string, error) {
		found, err := suggest(tui.Query{Domain: domain, Text: text, Limit: 8})
		var labels []string
		for _, suggestion := range found {
			labels = append(labels, suggestion.Label)
		}
		return labels, err
	}
	if _, err := ask(SuggestChats(dir), ChatDomain, ""); err == nil || !strings.HasPrefix(err.Error(), "No chats known yet") {
		t.Fatalf("no files: %v", err)
	}
	if _, err := ask(SuggestChats(""), ChatDomain, ""); err == nil || !strings.HasPrefix(err.Error(), "No chat directory") {
		t.Fatalf("no directory: %v", err)
	}
	chatdir.New("whatsapp-main", "whatsapp", 1, []chatdir.Chat{
		{Chat: chatdir.ChatRef("whatsapp-main", "1@g.us"), Title: "Family", Kind: chatdir.Group},
	}).Write(dir)
	chatdir.New("telegram-main", "telegram", 1, []chatdir.Chat{
		{Chat: chatdir.ChatRef("telegram-main", "telegram:user:7"), Title: "Family bot", Kind: chatdir.Direct},
	}).Write(dir)
	got, err := ask(SuggestChats(dir), ChatDomain, "fam")
	want := []string{"Family bot (direct, telegram-main:telegram:user:7)", "Family (group, whatsapp-main:1@g.us)"}
	if err != nil || !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v %v", got, err)
	}
	if got, _ := ask(SuggestChats(dir), "other", ""); got != nil {
		t.Fatalf("another domain: %v", got)
	}
}
