package telegramconnector

import (
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/gotd/td/tgerr"
)

func TestTelegramPermanentClassifiesRPCErrors(t *testing.T) {
	t.Parallel()
	if !telegramPermanent(&tgerr.Error{Code: 400, Type: "CHANNEL_INVALID"}) {
		t.Fatal("telegramPermanent() = false for RPC 400, want true")
	}
	if telegramPermanent(&tgerr.Error{Code: 420, Type: "FLOOD_WAIT"}) {
		t.Fatal("telegramPermanent() = true for RPC 420 flood wait, want false")
	}
	if telegramPermanent(errors.New("dial tcp: connection refused")) {
		t.Fatal("telegramPermanent() = true for a plain transient error, want false")
	}
}

func TestTelegramRetryAfterReadsTheFloodWait(t *testing.T) {
	t.Parallel()
	// As the reaction send wraps it.
	flood := fmt.Errorf("send Telegram reaction: %w", &tgerr.Error{Code: 420, Type: "FLOOD_WAIT", Argument: 5})
	if wait, ok := telegramRetryAfter(flood); !ok || wait != 6*time.Second {
		t.Fatalf("telegramRetryAfter(FLOOD_WAIT 5) = %v, %v; want 6s, true", wait, ok)
	}
	if _, ok := telegramRetryAfter(&tgerr.Error{Code: 400, Type: "CHANNEL_INVALID"}); ok {
		t.Fatal("telegramRetryAfter() = true for RPC 400, want false")
	}
	if _, ok := telegramRetryAfter(errors.New("dial tcp: connection refused")); ok {
		t.Fatal("telegramRetryAfter() = true for a plain error, want false")
	}
}
