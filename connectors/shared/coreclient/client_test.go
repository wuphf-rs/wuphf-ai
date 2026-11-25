package coreclient

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

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

func sendOpFor(sender Sender) sendOp {
	return func(ctx context.Context) (string, string, error) {
		return sender.SendMessage(ctx, &wuphfpb.MessageCreateDispatch{DispatchId: "d1"})
	}
}

func TestSendWithRetrySucceedsAfterTransientFailures(t *testing.T) {
	t.Parallel()
	sender := &flakySender{failures: maxSendAttempts - 1}
	id, threadID, err := Client{logger: zerolog.Nop()}.sendWithRetry(context.Background(), "d1", sendOpFor(sender))
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
	_, _, err := Client{logger: zerolog.Nop()}.sendWithRetry(context.Background(), "d1", sendOpFor(sender))
	if err == nil || sender.calls != maxSendAttempts {
		t.Fatalf("err = %v, calls = %d", err, sender.calls)
	}
}

func TestSendWithRetryStopsImmediatelyOnPermanentError(t *testing.T) {
	t.Parallel()
	sender := &permanentSender{}
	_, _, err := Client{logger: zerolog.Nop()}.sendWithRetry(context.Background(), "d1", sendOpFor(sender))
	if !isPermanent(err) {
		t.Fatalf("err = %v, want permanent", err)
	}
	if sender.calls != 1 {
		t.Fatalf("calls = %d, want 1 (no retry on permanent errors)", sender.calls)
	}
}

func TestSendWithRetryHonorsIsPermanentHook(t *testing.T) {
	t.Parallel()
	// A connector-supplied hook (e.g. Telegram's RPC-400 classifier) marks an
	// otherwise-unwrapped error as permanent, so the retry loop must stop after
	// the first attempt just as it does for coreclient.Permanent errors.
	fatal := errors.New("bad request")
	client := Client{
		opts:   Options{IsPermanent: func(err error) bool { return errors.Is(err, fatal) }},
		logger: zerolog.Nop(),
	}
	sender := &countingSender{err: fatal}
	_, _, err := client.sendWithRetry(context.Background(), "d1", sendOpFor(sender))
	if !errors.Is(err, fatal) {
		t.Fatalf("err = %v, want %v", err, fatal)
	}
	if !client.isPermanent(err) {
		t.Fatalf("client.isPermanent(%v) = false, want true", err)
	}
	if sender.calls != 1 {
		t.Fatalf("calls = %d, want 1 (hook-classified permanent errors are not retried)", sender.calls)
	}
}

// countingSender always fails with a fixed error and counts SendMessage calls.
type countingSender struct {
	err   error
	calls int
}

func (s *countingSender) SendMessage(context.Context, *wuphfpb.MessageCreateDispatch) (string, string, error) {
	s.calls++
	return "", "", s.err
}

func (s *countingSender) EditMessage(context.Context, *wuphfpb.MessageEditDispatch) (string, error) {
	return "", nil
}

func (s *countingSender) DeleteMessage(context.Context, *wuphfpb.MessageDeleteDispatch) (string, error) {
	return "", nil
}

func TestSendWithRetryStopsOnContextCancel(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	sender := &flakySender{failures: maxSendAttempts}
	_, _, err := Client{logger: zerolog.Nop()}.sendWithRetry(ctx, "d1", sendOpFor(sender))
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
}

func TestRegistrationCarriesTheDisappearingCapability(t *testing.T) {
	for _, supports := range []bool{true, false} {
		address, core := startRecordingCore(t)
		client := New(Options{
			CoreAddress:                address,
			ConnectorID:                "whatsapp-main",
			Platform:                   wuphfpb.Platform_PLATFORM_WHATSAPP,
			Token:                      "wa-secret",
			SupportsDisappearingTimers: supports,
		}, zerolog.Nop())
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		_ = client.runSession(ctx, &flakySender{}, make(chan *wuphfpb.ClientFrame), func() {})
		select {
		case registration := <-core.registrations:
			if got := registration.GetCapabilities().GetSupportsDisappearingTimers(); got != supports {
				t.Fatalf("supports_disappearing_timers = %v, want %v", got, supports)
			}
		case <-time.After(5 * time.Second):
			t.Fatal("the core saw no registration")
		}
		cancel()
	}
}

func TestRetryDelayWaitsAsLongAsThePlatformAsks(t *testing.T) {
	t.Parallel()
	limited := errors.New("rate limited")
	client := Client{opts: Options{RetryAfter: func(err error) (time.Duration, bool) {
		if errors.Is(err, limited) {
			return 7 * time.Second, true
		}
		return 0, false
	}}}
	cases := []struct {
		name string
		err  error
		want time.Duration
	}{
		{"asked for", limited, 7 * time.Second},
		{"not a rate limit", errors.New("transient"), sendRetryDelay},
	}
	for _, tc := range cases {
		if got := client.retryDelay(tc.err); got != tc.want {
			t.Errorf("%s: retryDelay = %v, want %v", tc.name, got, tc.want)
		}
	}
	// A very long wait is capped; a hook-less client keeps the fixed delay.
	long := Client{opts: Options{RetryAfter: func(error) (time.Duration, bool) { return time.Hour, true }}}
	if got := long.retryDelay(limited); got != maxRetryAfter {
		t.Errorf("capped: retryDelay = %v, want %v", got, maxRetryAfter)
	}
	if got := (Client{}).retryDelay(limited); got != sendRetryDelay {
		t.Errorf("no hook: retryDelay = %v, want %v", got, sendRetryDelay)
	}
}
