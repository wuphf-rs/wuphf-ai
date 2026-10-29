package logging

import (
	"bytes"
	"testing"

	"github.com/rs/zerolog"
)

// Not parallel: the level is process-wide.
func TestLogLevelChangeIsApplied(t *testing.T) {
	t.Setenv(envLevel, "")
	t.Cleanup(func() { zerolog.SetGlobalLevel(zerolog.TraceLevel) })

	_ = New("info")
	var output bytes.Buffer
	// A logger derived before the change follows it too.
	derived := zerolog.New(&output).With().Str("component", "sender").Logger()
	derived.Debug().Msg("hidden")
	if output.Len() != 0 {
		t.Fatalf("debug logged at info: %q", output.String())
	}

	SetLevel("debug")
	derived.Debug().Msg("shown")
	if !bytes.Contains(output.Bytes(), []byte("shown")) {
		t.Fatalf("debug not logged after the change: %q", output.String())
	}
}

func TestEnvironmentLevelWins(t *testing.T) {
	t.Setenv(envLevel, "warn")
	t.Cleanup(func() { zerolog.SetGlobalLevel(zerolog.TraceLevel) })

	SetLevel("debug")
	if zerolog.GlobalLevel() != zerolog.WarnLevel {
		t.Fatalf("level = %s, want warn", zerolog.GlobalLevel())
	}
}

func TestCheckLevel(t *testing.T) {
	for _, level := range []string{"", "info", "DEBUG", "warn"} {
		if err := CheckLevel(level); err != nil {
			t.Errorf("CheckLevel(%q) = %v", level, err)
		}
	}
	for _, level := range []string{"verbose", "fatal"} {
		if err := CheckLevel(level); err == nil {
			t.Errorf("CheckLevel(%q) accepted", level)
		}
	}
}
