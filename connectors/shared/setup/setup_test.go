package setup

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/propheci/botconf/go/describe"
	"github.com/propheci/botconf/go/tui"
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
