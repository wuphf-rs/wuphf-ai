package core

import (
	"context"
	"errors"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
)

type flakySender struct {
	failures int
	calls    int
}

func (s *flakySender) SendMessage(context.Context, *wuphfpb.MessageCreateDispatch) (string, error) {
	s.calls++
	if s.calls <= s.failures {
		return "", errors.New("transient")
	}
	return "platform-id", nil
}

func TestSendWithRetrySucceedsAfterTransientFailures(t *testing.T) {
	t.Parallel()
	sender := &flakySender{failures: maxSendAttempts - 1}
	id, err := sendWithRetry(context.Background(), sender, &wuphfpb.MessageCreateDispatch{DispatchId: "d1"})
	if err != nil {
		t.Fatalf("sendWithRetry() error = %v", err)
	}
	if id != "platform-id" || sender.calls != maxSendAttempts {
		t.Fatalf("id = %q, calls = %d", id, sender.calls)
	}
}

func TestSendWithRetryGivesUpAfterMaxAttempts(t *testing.T) {
	t.Parallel()
	sender := &flakySender{failures: maxSendAttempts}
	_, err := sendWithRetry(context.Background(), sender, &wuphfpb.MessageCreateDispatch{DispatchId: "d1"})
	if err == nil || sender.calls != maxSendAttempts {
		t.Fatalf("err = %v, calls = %d", err, sender.calls)
	}
}

func TestSendWithRetryStopsOnContextCancel(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	sender := &flakySender{failures: maxSendAttempts}
	_, err := sendWithRetry(ctx, sender, &wuphfpb.MessageCreateDispatch{DispatchId: "d1"})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
}
