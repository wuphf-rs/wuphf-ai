package version

import (
	"bytes"
	"testing"
)

func TestTheVersionIsRequestedByItsFirstArgument(t *testing.T) {
	for _, args := range [][]string{{"version"}, {"--version"}, {"-version", "x"}} {
		if !Requested(args) {
			t.Errorf("Requested(%q) = false", args)
		}
	}
	for _, args := range [][]string{nil, {"run"}, {"--config", "version"}} {
		if Requested(args) {
			t.Errorf("Requested(%q) = true", args)
		}
	}
}

func TestTheVersionIsPrintedAfterTheName(t *testing.T) {
	var out bytes.Buffer
	write(&out, "wuphf-telegram")
	if got := out.String(); got != "wuphf-telegram dev\n" {
		t.Fatalf("printed %q", got)
	}
}
