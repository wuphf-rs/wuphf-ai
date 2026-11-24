package main

import (
	"fmt"
	"testing"

	"github.com/wuphf-rs/wuphf-ai/misc/internal/config"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// A core that is down or starting is tried again; one that refuses is not.
func TestOnlyAnUnreachableCoreIsRetried(t *testing.T) {
	t.Parallel()
	wrapped := func(code codes.Code) error {
		return fmt.Errorf("declare the settings to the core: %w", status.Error(code, "x"))
	}
	for code, want := range map[codes.Code]bool{
		codes.Unavailable:      true,
		codes.DeadlineExceeded: true,
		codes.Unauthenticated:  false,
		codes.Unimplemented:    false,
		codes.InvalidArgument:  false,
	} {
		if got := retryable(wrapped(code)); got != want {
			t.Errorf("retryable(%v) = %v, want %v", code, got, want)
		}
	}
}

func TestEnabledFeatures(t *testing.T) {
	t.Parallel()
	if got := enabledFeatures(config.Runtime{}); len(got) != 0 {
		t.Fatalf("none enabled: %v", got)
	}
	got := enabledFeatures(config.Runtime{Instagram: config.Instagram{Enabled: true}})
	if len(got) != 1 || got[0] != "instagram" {
		t.Fatalf("instagram enabled: %v", got)
	}
}
