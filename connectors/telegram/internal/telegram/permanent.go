package telegramconnector

import (
	"errors"
	"time"

	"github.com/gotd/td/tgerr"
)

// telegramPermanent classifies Telegram-specific send failures the shared core
// client cannot: a Telegram RPC 400 means the request itself is invalid (a bad
// peer, an empty message), so retrying or redelivering cannot fix it.
// FLOOD_WAIT (420) is waited out (telegramRetryAfter); server errors (5xx) are
// transient and left to the reconnect-and-redeliver path. It is passed to
// coreclient as Options.IsPermanent.
func telegramPermanent(err error) bool {
	var rpcErr *tgerr.Error
	if errors.As(err, &rpcErr) {
		return rpcErr.Code == 400
	}
	return false
}

// telegramRetryAfter reads how long a FLOOD_WAIT (or FLOOD_PREMIUM_WAIT) asks to
// wait, plus a second, as gotd's own waiter does: Telegram counts the wait in
// whole seconds. Retrying sooner only meets the limit again. It is passed to
// coreclient as Options.RetryAfter.
func telegramRetryAfter(err error) (time.Duration, bool) {
	wait, ok := tgerr.AsFloodWait(err)
	if !ok {
		return 0, false
	}
	return wait + time.Second, true
}
