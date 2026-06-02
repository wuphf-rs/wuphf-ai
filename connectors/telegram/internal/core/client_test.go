package core

import (
	"context"
	"errors"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/tgerr"
)

type permanentSender struct{ calls int }

func (s *permanentSender) SendMessage(context.Context, *wuphfpb.MessageCreateDispatch) (string, string, error) {
	s.calls++
	return "", "", Permanent(errors.New("invalid target"))
}

func (s *permanentSender) EditMessage(context.Context, *wuphfpb.MessageEditDispatch) (string, error) {
	return "", nil
}

func (s *permanentSender) DeleteMessage(context.Context, *wuphfpb.MessageDeleteDispatch) (string, error) {
	return "", nil
}

type flakySender struct {
	failures int
	calls    int
}

func (s *flakySender) SendMessage(context.Context, *wuphfpb.MessageCreateDispatch) (string, string, error) {
	s.calls++
	if s.calls <= s.failures {
		return "", "", errors.New("transient")
	}
	return "platform-id", "topic-id", nil
}

func (s *flakySender) EditMessage(context.Context, *wuphfpb.MessageEditDispatch) (string, error) {
	return "", nil
}

func (s *flakySender) DeleteMessage(context.Context, *wuphfpb.MessageDeleteDispatch) (string, error) {
	return "", nil
}

func sendOpFor(sender Sender) sendOp {
	return func(ctx context.Context) (string, string, error) {
		return sender.SendMessage(ctx, &wuphfpb.MessageCreateDispatch{DispatchId: "d1"})
	}
}

func TestSendWithRetrySucceedsAfterTransientFailures(t *testing.T) {
	t.Parallel()
	sender := &flakySender{failures: maxSendAttempts - 1}
	id, threadID, err := sendWithRetry(context.Background(), "d1", sendOpFor(sender))
	if err != nil {
		t.Fatalf("sendWithRetry() error = %v", err)
	}
	if id != "platform-id" || threadID != "topic-id" || sender.calls != maxSendAttempts {
		t.Fatalf("id = %q, threadID = %q, calls = %d", id, threadID, sender.calls)
	}
}

func TestSendWithRetryGivesUpAfterMaxAttempts(t *testing.T) {
	t.Parallel()
	sender := &flakySender{failures: maxSendAttempts}
	_, _, err := sendWithRetry(context.Background(), "d1", sendOpFor(sender))
	if err == nil || sender.calls != maxSendAttempts {
		t.Fatalf("err = %v, calls = %d", err, sender.calls)
	}
}

func TestSendWithRetryDoesNotRetryPermanentFailures(t *testing.T) {
	t.Parallel()
	sender := &permanentSender{}
	_, _, err := sendWithRetry(context.Background(), "d1", sendOpFor(sender))
	if err == nil || !isPermanent(err) || sender.calls != 1 {
		t.Fatalf("err = %v, permanent = %v, calls = %d", err, isPermanent(err), sender.calls)
	}
}

func TestIsPermanentClassifiesTelegramRPCErrors(t *testing.T) {
	t.Parallel()
	if !isPermanent(&tgerr.Error{Code: 400, Type: "CHANNEL_INVALID"}) {
		t.Fatal("isPermanent() = false for RPC 400, want true")
	}
	if isPermanent(&tgerr.Error{Code: 420, Type: "FLOOD_WAIT"}) {
		t.Fatal("isPermanent() = true for RPC 420 flood wait, want false")
	}
	if isPermanent(errors.New("dial tcp: connection refused")) {
		t.Fatal("isPermanent() = true for a plain transient error, want false")
	}
}

func TestSendWithRetryStopsOnContextCancel(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	sender := &flakySender{failures: maxSendAttempts}
	_, _, err := sendWithRetry(ctx, "d1", sendOpFor(sender))
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
}
