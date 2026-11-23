package telegramconnector

import (
	"context"
	"math"
	"testing"
	"time"

	"github.com/gotd/td/tg"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/protobuf/proto"
)

var forum = &tg.PeerChannel{ChannelID: 42}

func TestTopicPinsBecomeChatUpdates(t *testing.T) {
	updates := pinnedTopicUpdates(&tg.UpdatePinnedForumTopic{Peer: forum, TopicID: 57, Pinned: true})
	if len(updates) != 1 || updates[0].GetChatId() != "telegram:channel:42" ||
		updates[0].GetThreadId() != "57" || !updates[0].GetPinned() {
		t.Fatalf("pin = %v, want topic 57 pinned", updates)
	}
	updates = pinnedTopicUpdates(&tg.UpdatePinnedForumTopic{Peer: forum, TopicID: 57})
	if updates[0].Pinned == nil || updates[0].GetPinned() {
		t.Fatalf("unpin = %v, want pinned false", updates)
	}

	bulk := pinnedTopicsUpdates(&tg.UpdatePinnedForumTopics{Peer: forum, Order: []int{57, 58}})
	if len(bulk) != 2 || bulk[0].GetThreadId() != "57" || bulk[1].GetThreadId() != "58" ||
		!bulk[0].GetPinned() || !bulk[1].GetPinned() {
		t.Fatalf("bulk = %v, want both topics pinned", bulk)
	}
}

func notify(peer tg.NotifyPeerClass, until *int) *tg.UpdateNotifySettings {
	settings := tg.PeerNotifySettings{}
	if until != nil {
		settings.SetMuteUntil(*until)
	}
	return &tg.UpdateNotifySettings{Peer: peer, NotifySettings: settings}
}

func TestTopicMutesBecomeChatUpdates(t *testing.T) {
	now := time.Unix(1_800_000_000, 0)
	topic := &tg.NotifyForumTopic{Peer: forum, TopMsgID: 57}
	value := func(v int) *int { return &v }
	for _, c := range []struct {
		name  string
		until *int
		want  int64
	}{
		{"not set", nil, 0},
		{"over", value(1_700_000_000), 0},
		{"until a time", value(1_900_000_000), 1_900_000_000},
		{"forever", value(math.MaxInt32), -1},
	} {
		updates := notifyUpdates(notify(topic, c.until), false, now)
		if len(updates) != 1 || updates[0].GetThreadId() != "57" || updates[0].GetMute().GetMutedUntil() != c.want {
			t.Errorf("%s: %v, want mute %d", c.name, updates, c.want)
		}
	}
	if updates := notifyUpdates(notify(&tg.NotifyPeer{Peer: forum}, value(0)), false, now); updates != nil {
		t.Fatalf("a chat's (not a topic's) notify setting reported %v", updates)
	}
	if updates := notifyUpdates(notify(topic, value(1_900_000_000)), true, now); updates != nil {
		t.Fatalf("a bot session reported a mute: %v", updates)
	}
}

func TestTopicCloseBecomesArchived(t *testing.T) {
	closeEdit := func(closed bool) tg.MessageClass {
		action := &tg.MessageActionTopicEdit{}
		action.SetClosed(closed)
		reply := &tg.MessageReplyHeader{ForumTopic: true}
		reply.SetReplyToMsgID(57)
		message := &tg.MessageService{ID: 900, PeerID: forum, Action: action}
		message.SetReplyTo(reply)
		return message
	}
	for _, closed := range []bool{true, false} {
		updates := topicCloseUpdates(closeEdit(closed))
		if len(updates) != 1 || updates[0].GetThreadId() != "57" || updates[0].Archived == nil ||
			updates[0].GetArchived() != closed {
			t.Fatalf("closed=%v: %v", closed, updates)
		}
	}
	rename := &tg.MessageActionTopicEdit{}
	rename.SetTitle("New name")
	if updates := topicCloseUpdates(&tg.MessageService{PeerID: forum, Action: rename}); updates != nil {
		t.Fatalf("a rename reported %v", updates)
	}
	if updates := topicCloseUpdates(&tg.Message{PeerID: forum}); updates != nil {
		t.Fatalf("a plain message reported %v", updates)
	}
}

func stateDispatch(thread *string) *wuphfpb.ChatStateDispatch {
	return &wuphfpb.ChatStateDispatch{
		DispatchId:     "d-1",
		TargetChatId:   "telegram:chat:42",
		TargetThreadId: thread,
		Pinned:         proto.Bool(true),
		Archived:       proto.Bool(true),
		Mute:           &wuphfpb.MuteState{MutedUntil: -1},
	}
}

func TestApplyChatStateSetsTheTopic(t *testing.T) {
	api := &recordingAPI{}
	sender := NewSender(tg.NewClient(api), NewPeerCache(), nil, nil, nil, false)

	chatID, err := sender.ApplyChatState(context.Background(), stateDispatch(proto.String("57")))
	if err != nil || chatID != "telegram:chat:42" {
		t.Fatalf("ApplyChatState() = %q, %v", chatID, err)
	}
	pins := sent[*tg.MessagesUpdatePinnedForumTopicRequest](api)
	if len(pins) != 1 || pins[0].TopicID != 57 || !pins[0].Pinned {
		t.Fatalf("pins = %#v, want topic 57 pinned", pins)
	}
	edits := sent[*tg.MessagesEditForumTopicRequest](api)
	if closed, ok := edits[0].GetClosed(); len(edits) != 1 || edits[0].TopicID != 57 || !ok || !closed {
		t.Fatalf("edits = %#v, want topic 57 closed", edits)
	}
	mutes := sent[*tg.AccountUpdateNotifySettingsRequest](api)
	if len(mutes) != 1 {
		t.Fatalf("mutes = %#v, want one", mutes)
	}
	topic, ok := mutes[0].Peer.(*tg.InputNotifyForumTopic)
	until, set := mutes[0].Settings.GetMuteUntil()
	if !ok || topic.TopMsgID != 57 || !set || until != math.MaxInt32 {
		t.Fatalf("mute = %#v, want topic 57 muted forever (MaxInt32)", mutes[0])
	}
}

func TestApplyChatStateWithoutAThreadDoesNothing(t *testing.T) {
	api := &recordingAPI{}
	sender := NewSender(tg.NewClient(api), NewPeerCache(), nil, nil, nil, false)
	chatID, err := sender.ApplyChatState(context.Background(), stateDispatch(nil))
	if err != nil || chatID != "telegram:chat:42" {
		t.Fatalf("ApplyChatState() = %q, %v", chatID, err)
	}
	if len(api.requests) != 0 {
		t.Fatalf("a plain chat got requests: %#v", api.requests)
	}
}

func TestABotDoesNotMute(t *testing.T) {
	api := &recordingAPI{}
	sender := NewSender(tg.NewClient(api), NewPeerCache(), nil, nil, nil, true)
	if _, err := sender.ApplyChatState(context.Background(), stateDispatch(proto.String("57"))); err != nil {
		t.Fatal(err)
	}
	if mutes := sent[*tg.AccountUpdateNotifySettingsRequest](api); len(mutes) != 0 {
		t.Fatalf("a bot sent notify settings: %#v", mutes)
	}
	if pins := sent[*tg.MessagesUpdatePinnedForumTopicRequest](api); len(pins) != 1 {
		t.Fatalf("a bot still pins: %#v", pins)
	}
}
