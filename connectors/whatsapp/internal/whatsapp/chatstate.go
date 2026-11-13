package whatsappconnector

import (
	"context"
	"fmt"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/appstate"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// historyMillisThreshold separates a history sync's mute end in milliseconds
// from one in seconds: the unit is undocumented and whatsmeow never reads it,
// and any date before the year 5138 in seconds is below it.
const historyMillisThreshold = 100_000_000_000

// muteFromWhatsApp maps WhatsApp's mute to the bridge's: 0 not muted (or a
// mute already over), -1 forever (a negative end, or no end at all, as
// whatsmeow's BuildMuteAbs and the Rust connector read it), otherwise the end
// in Unix seconds. WhatsApp reports the end in milliseconds; a mute the bridge sets
// is exactly `muted_until * 1000`, so its echo divides back to the same value.
func muteFromWhatsApp(muted bool, endMillis int64, now time.Time) int64 {
	switch {
	case !muted:
		return 0
	case endMillis <= 0:
		return -1
	}
	seconds := endMillis / 1000
	if seconds <= now.Unix() {
		return 0
	}
	return seconds
}

// muteFromHistory maps a history sync's mute end, in milliseconds or seconds,
// to the bridge's mute.
func muteFromHistory(end uint64, now time.Time) int64 {
	if end == 0 {
		return 0
	}
	if end > historyMillisThreshold {
		end /= 1000
	}
	if int64(end) <= now.Unix() {
		return 0
	}
	return int64(end)
}

func (n Normalizer) chatState(ctx context.Context, chat types.JID, snapshot bool, set func(*wuphfpb.ChatUpdate)) error {
	update := &wuphfpb.ChatUpdate{ChatId: n.canonical(ctx, chat).String()}
	set(update)
	return n.sendChatUpdates(ctx, []*wuphfpb.ChatUpdate{update}, snapshot)
}

// HandlePin reports a chat pinned or unpinned; a full app-state sync is a
// snapshot.
func (n Normalizer) HandlePin(ctx context.Context, evt *events.Pin) error {
	pinned := evt.Action.GetPinned()
	return n.chatState(ctx, evt.JID, evt.FromFullSync, func(update *wuphfpb.ChatUpdate) {
		update.Pinned = &pinned
	})
}

// HandleMute reports a chat's mute.
func (n Normalizer) HandleMute(ctx context.Context, evt *events.Mute) error {
	mute := muteFromWhatsApp(evt.Action.GetMuted(), evt.Action.GetMuteEndTimestamp(), time.Now())
	return n.chatState(ctx, evt.JID, evt.FromFullSync, func(update *wuphfpb.ChatUpdate) {
		update.Mute = &wuphfpb.MuteState{MutedUntil: mute}
	})
}

// HandleArchive reports a chat archived or unarchived.
func (n Normalizer) HandleArchive(ctx context.Context, evt *events.Archive) error {
	archived := evt.Action.GetArchived()
	return n.chatState(ctx, evt.JID, evt.FromFullSync, func(update *wuphfpb.ChatUpdate) {
		update.Archived = &archived
	})
}

// appStateSender is the slice of the whatsmeow client that changes chat
// state.
type appStateSender interface {
	SendAppState(ctx context.Context, patch appstate.PatchInfo) error
}

// ApplyChatState brings the chat to the dispatch's pin, mute and archive
// state, one app-state patch per field, and returns the target chat id. A mute
// end is set as exactly `muted_until * 1000` milliseconds, so WhatsApp's echo
// maps back to the same value; one already over unmutes. Archiving also
// unpins, as WhatsApp does; that unpin comes back as its own report.
func (s Sender) ApplyChatState(ctx context.Context, dispatch *wuphfpb.ChatStateDispatch) (string, error) {
	chatID := dispatch.GetTargetChatId()
	chat, _, err := s.target(ctx, chatID)
	if err != nil {
		return "", err
	}
	var patches []appstate.PatchInfo
	if dispatch.Pinned != nil {
		patches = append(patches, appstate.BuildPin(chat, dispatch.GetPinned()))
	}
	if dispatch.Mute != nil {
		patches = append(patches, mutePatch(chat, dispatch.GetMute().GetMutedUntil(), time.Now()))
	}
	if dispatch.Archived != nil {
		patches = append(patches, appstate.BuildArchive(chat, dispatch.GetArchived(), time.Time{}, nil))
	}
	for _, patch := range patches {
		if err := s.client.SendAppState(ctx, patch); err != nil {
			return "", fmt.Errorf("send WhatsApp chat state: %w", err)
		}
	}
	return chatID, nil
}

// mutePatch is the app-state patch for a bridge mute: forever (-1) with no
// end, 0 or an end already over unmutes, otherwise the end in milliseconds.
func mutePatch(chat types.JID, mutedUntil int64, now time.Time) appstate.PatchInfo {
	switch {
	case mutedUntil < 0:
		return appstate.BuildMuteAbs(chat, true, nil)
	case mutedUntil == 0 || mutedUntil <= now.Unix():
		return appstate.BuildMuteAbs(chat, false, nil)
	default:
		end := mutedUntil * 1000
		return appstate.BuildMuteAbs(chat, true, &end)
	}
}
