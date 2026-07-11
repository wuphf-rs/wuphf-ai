package telegramconnector

import (
	"errors"
	"testing"

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
