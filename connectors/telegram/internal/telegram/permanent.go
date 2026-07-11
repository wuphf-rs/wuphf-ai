package telegramconnector

import (
	"errors"

	"github.com/gotd/td/tgerr"
)

// telegramPermanent classifies Telegram-specific send failures the shared core
// client cannot: a Telegram RPC 400 means the request itself is invalid (a bad
// peer, an empty message), so retrying or redelivering cannot fix it.
// FLOOD_WAIT (420) and server errors (5xx) are transient and left to the
// reconnect-and-redeliver path. It is passed to coreclient as Options.IsPermanent.
func telegramPermanent(err error) bool {
	var rpcErr *tgerr.Error
	if errors.As(err, &rpcErr) {
		return rpcErr.Code == 400
	}
	return false
}
