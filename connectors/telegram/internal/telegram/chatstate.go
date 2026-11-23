package telegramconnector

import (
	"context"
	"fmt"
	"math"
	"strconv"
	"time"

	"github.com/gotd/td/tg"
	"github.com/gotd/td/tgerr"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// telegramMutedForever is the mute_until value clients use for "muted
// forever" (int32 max); the bridge calls it -1.
const telegramMutedForever = math.MaxInt32

// IsBot reports whether the session is a bot (bot addressing is on).
func (c *PeerCache) IsBot() bool {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.bot
}

// muteFromTelegram maps a topic's notify setting to the bridge's mute: no
// mute or one already over is 0, Telegram's "forever" is -1, otherwise the
// end in Unix seconds.
func muteFromTelegram(until int, set bool, now time.Time) int64 {
	switch {
	case !set || int64(until) <= now.Unix():
		return 0
	case until == telegramMutedForever:
		return -1
	default:
		return int64(until)
	}
}

// muteToTelegram is muteFromTelegram's inverse: -1 is Telegram's "forever".
func muteToTelegram(mutedUntil int64) int {
	if mutedUntil < 0 {
		return telegramMutedForever
	}
	return int(mutedUntil)
}

// topicState is a ChatUpdate about one forum topic's state.
func topicState(peer tg.PeerClass, topicID int) *wuphfpb.ChatUpdate {
	thread := strconv.Itoa(topicID)
	return &wuphfpb.ChatUpdate{ChatId: formatPeer(peer), ThreadId: &thread}
}

func (n Normalizer) sendChatUpdates(ctx context.Context, updates []*wuphfpb.ChatUpdate) error {
	if len(updates) == 0 {
		return nil
	}
	select {
	case n.inputs <- &wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_ChatUpdate{
		ChatUpdate: &wuphfpb.ChatUpdateEvent{Updates: updates},
	}}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// pinnedTopicUpdates reports a topic pinned or unpinned.
func pinnedTopicUpdates(update *tg.UpdatePinnedForumTopic) []*wuphfpb.ChatUpdate {
	state := topicState(update.Peer, update.TopicID)
	pinned := update.Pinned
	state.Pinned = &pinned
	return []*wuphfpb.ChatUpdate{state}
}

// pinnedTopicsUpdates reports each topic of a bulk reorder as pinned. An unpin
// hidden in a reorder is not detected: the connector keeps no topic state.
func pinnedTopicsUpdates(update *tg.UpdatePinnedForumTopics) []*wuphfpb.ChatUpdate {
	updates := make([]*wuphfpb.ChatUpdate, 0, len(update.Order))
	for _, topicID := range update.Order {
		state := topicState(update.Peer, topicID)
		pinned := true
		state.Pinned = &pinned
		updates = append(updates, state)
	}
	return updates
}

// notifyUpdates reports a topic's mute. Only a topic's settings count, and
// only a user session's: a bot's notify settings mean nothing.
func notifyUpdates(update *tg.UpdateNotifySettings, bot bool, now time.Time) []*wuphfpb.ChatUpdate {
	topic, ok := update.Peer.(*tg.NotifyForumTopic)
	if !ok || bot {
		return nil
	}
	state := topicState(topic.Peer, topic.TopMsgID)
	until, set := update.NotifySettings.GetMuteUntil()
	state.Mute = &wuphfpb.MuteState{MutedUntil: muteFromTelegram(until, set, now)}
	return []*wuphfpb.ChatUpdate{state}
}

// topicCloseUpdates reports a topic closed or reopened: a service message
// whose action is a topic edit that sets `closed`, posted in the topic.
func topicCloseUpdates(message tg.MessageClass) []*wuphfpb.ChatUpdate {
	service, ok := message.(*tg.MessageService)
	if !ok {
		return nil
	}
	edit, ok := service.Action.(*tg.MessageActionTopicEdit)
	if !ok {
		return nil
	}
	closed, ok := edit.GetClosed()
	if !ok {
		return nil
	}
	reply, ok := service.GetReplyTo()
	if !ok {
		return nil
	}
	topicID, ok := topicOf(reply)
	if !ok {
		return nil
	}
	state := topicState(service.PeerID, topicID)
	state.Archived = &closed
	return []*wuphfpb.ChatUpdate{state}
}

// ApplyChatState brings a forum topic to the dispatch's state: pin, close
// (archive) and, for a user session, mute. A dispatch without a thread names
// a plain chat, which is out of scope, so it is acknowledged without acting;
// so is a mute on a bot session. A state the topic already has counts as
// done. Returns the target chat id; the echo comes back as an update.
func (s Sender) ApplyChatState(ctx context.Context, dispatch *wuphfpb.ChatStateDispatch) (string, error) {
	chatID := dispatch.GetTargetChatId()
	if dispatch.GetTargetThreadId() == "" {
		return chatID, nil
	}
	topicID, err := strconv.Atoi(dispatch.GetTargetThreadId())
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram topic id %q: %w", dispatch.GetTargetThreadId(), err))
	}
	peer, err := s.peers.InputPeer(ctx, chatID)
	if err != nil {
		return "", err
	}
	if dispatch.Pinned != nil {
		_, err := s.api.MessagesUpdatePinnedForumTopic(ctx, &tg.MessagesUpdatePinnedForumTopicRequest{
			Peer: peer, TopicID: topicID, Pinned: dispatch.GetPinned(),
		})
		if err := alreadyDone(err); err != nil {
			return "", fmt.Errorf("pin Telegram topic: %w", err)
		}
	}
	if dispatch.Archived != nil {
		request := &tg.MessagesEditForumTopicRequest{Peer: peer, TopicID: topicID}
		request.SetClosed(dispatch.GetArchived())
		_, err := s.api.MessagesEditForumTopic(ctx, request)
		if err := alreadyDone(err); err != nil {
			return "", fmt.Errorf("close Telegram topic: %w", err)
		}
	}
	if dispatch.Mute != nil && !s.bot {
		settings := tg.InputPeerNotifySettings{}
		settings.SetMuteUntil(muteToTelegram(dispatch.GetMute().GetMutedUntil()))
		_, err := s.api.AccountUpdateNotifySettings(ctx, &tg.AccountUpdateNotifySettingsRequest{
			Peer:     &tg.InputNotifyForumTopic{Peer: peer, TopMsgID: topicID},
			Settings: settings,
		})
		if err != nil {
			return "", fmt.Errorf("mute Telegram topic: %w", err)
		}
	}
	return chatID, nil
}

// alreadyDone treats "the topic already has this state" as success.
func alreadyDone(err error) error {
	if tgerr.Is(err, "TOPIC_NOT_MODIFIED", "PINNED_TOPIC_NOT_MODIFIED") {
		return nil
	}
	return err
}
