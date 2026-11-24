package telegramconnector

import (
	"context"
	"errors"
	"testing"

	"github.com/gotd/td/tg"
	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// fakeReactionAPI answers the reaction lookups and counts them.
type fakeReactionAPI struct {
	list      *tg.MessagesMessageReactionsList
	listErr   error
	lists     int
	emoji     map[int64]string
	emojiHits int
	messages  []tg.MessageClass
	fetches   int
}

func (f *fakeReactionAPI) MessagesGetMessageReactionsList(context.Context, *tg.MessagesGetMessageReactionsListRequest) (*tg.MessagesMessageReactionsList, error) {
	f.lists++
	return f.list, f.listErr
}

func (f *fakeReactionAPI) MessagesGetCustomEmojiDocuments(_ context.Context, ids []int64) ([]tg.DocumentClass, error) {
	f.emojiHits++
	var documents []tg.DocumentClass
	for _, id := range ids {
		if alt, ok := f.emoji[id]; ok {
			documents = append(documents, &tg.Document{
				ID:         id,
				Attributes: []tg.DocumentAttributeClass{&tg.DocumentAttributeCustomEmoji{Alt: alt}},
			})
		}
	}
	return documents, nil
}

func (f *fakeReactionAPI) ChannelsGetMessages(context.Context, *tg.ChannelsGetMessagesRequest) (tg.MessagesMessagesClass, error) {
	f.fetches++
	return &tg.MessagesChannelMessages{Messages: f.messages}, nil
}

const (
	reactionChannel = 99
	ownerUser       = 704338780
	bridgeUser      = 5550001
)

func newTestReactions(api *fakeReactionAPI) *Reactions {
	peers := NewPeerCache()
	channel := &tg.Channel{ID: reactionChannel}
	channel.SetAccessHash(1)
	peers.Observe(tg.Entities{Channels: map[int64]*tg.Channel{reactionChannel: channel}})
	reactions := NewReactions(peers, zerolog.Nop())
	reactions.SetAPI(api)
	reactions.SetSelf(bridgeUser)
	return reactions
}

func emojiReaction(emoji string) tg.ReactionClass { return &tg.ReactionEmoji{Emoticon: emoji} }

func peerReaction(user int64, reaction tg.ReactionClass, date int, my bool) tg.MessagePeerReaction {
	return tg.MessagePeerReaction{PeerID: &tg.PeerUser{UserID: user}, Reaction: reaction, Date: date, My: my}
}

func reactionsUpdate(recent []tg.MessagePeerReaction, total int) *tg.UpdateMessageReactions {
	update := &tg.UpdateMessageReactions{
		Peer:  &tg.PeerChannel{ChannelID: reactionChannel},
		MsgID: 42,
		Reactions: tg.MessageReactions{
			CanSeeList:      true,
			RecentReactions: recent,
		},
	}
	if total > 0 {
		update.Reactions.Results = []tg.ReactionCount{{Reaction: emojiReaction("❤️"), Count: total}}
	}
	update.SetTopMsgID(12)
	return update
}

// reactionSet checks a report is a reaction set: reactionSet(t)(report).
func reactionSet(t *testing.T) func(*wuphfpb.ClientFrame, bool) *wuphfpb.ReactionsEvent {
	t.Helper()
	return func(frame *wuphfpb.ClientFrame, ok bool) *wuphfpb.ReactionsEvent {
		t.Helper()
		if !ok {
			t.Fatal("no reaction set reported")
		}
		set := frame.GetReactionsSet()
		if set == nil {
			t.Fatalf("frame = %v, want a reaction set", frame)
		}
		return set
	}
}

func TestReactionSetKeepsEachReactorsNewestAndLeavesOutTheBridge(t *testing.T) {
	t.Parallel()
	reactions := newTestReactions(&fakeReactionAPI{})
	owner := &tg.User{ID: ownerUser, FirstName: "Akshett"}
	update := reactionsUpdate([]tg.MessagePeerReaction{
		peerReaction(ownerUser, emojiReaction("👍"), 1, false),
		peerReaction(ownerUser, emojiReaction("❤️"), 3, false),
		// The bridge's delivered signal, by this session's own account.
		peerReaction(bridgeUser, emojiReaction("👍"), 2, true),
		// A paid star has no emoji to report.
		peerReaction(ownerUser, &tg.ReactionPaid{}, 4, false),
	}, 4)

	set := reactionSet(t)(reactions.messageReactions(context.Background(), tg.Entities{Users: map[int64]*tg.User{ownerUser: owner}}, update))
	if set.ChatId != "telegram:channel:99" || set.PlatformMessageId != "42" || set.GetThreadId() != "12" {
		t.Fatalf("address = %s/%s/%s", set.ChatId, set.GetThreadId(), set.PlatformMessageId)
	}
	if len(set.Reactions) != 1 {
		t.Fatalf("reactions = %v, want only the owner's", set.Reactions)
	}
	got := set.Reactions[0]
	if got.Sender != "704338780" || got.Emoji != "❤️" || got.GetSenderName() != "Akshett" {
		t.Fatalf("reaction = %v, want the owner's newest ❤️ by plain user id", got)
	}
}

func TestAnEmptyReactionSetIsReported(t *testing.T) {
	t.Parallel()
	reactions := newTestReactions(&fakeReactionAPI{})
	set := reactionSet(t)(reactions.messageReactions(context.Background(), tg.Entities{}, reactionsUpdate(nil, 0)))
	if len(set.Reactions) != 0 {
		t.Fatalf("reactions = %v, want none", set.Reactions)
	}
}

func TestAnIncompleteUpdateReadsTheReactionList(t *testing.T) {
	t.Parallel()
	api := &fakeReactionAPI{list: &tg.MessagesMessageReactionsList{
		Reactions: []tg.MessagePeerReaction{peerReaction(ownerUser, emojiReaction("😂"), 1, false)},
		Users:     []tg.UserClass{&tg.User{ID: ownerUser, FirstName: "Akshett"}},
	}}
	reactions := newTestReactions(api)

	set := reactionSet(t)(reactions.messageReactions(context.Background(), tg.Entities{}, reactionsUpdate(nil, 1)))
	if api.lists != 1 || len(set.Reactions) != 1 || set.Reactions[0].Emoji != "😂" || set.Reactions[0].GetSenderName() != "Akshett" {
		t.Fatalf("lists = %d, reactions = %v", api.lists, set.Reactions)
	}
}

func TestAReactionSetThatCannotBeReadIsNotReported(t *testing.T) {
	t.Parallel()
	reactions := newTestReactions(&fakeReactionAPI{listErr: errors.New("CHAT_ADMIN_REQUIRED")})
	if frame, ok := reactions.messageReactions(context.Background(), tg.Entities{}, reactionsUpdate(nil, 1)); ok {
		t.Fatalf("reported %v from an unreadable list; a partial set reads as clears", frame)
	}
}

func TestACustomEmojiIsReportedAsItsStandardEmoji(t *testing.T) {
	t.Parallel()
	api := &fakeReactionAPI{emoji: map[int64]string{7: "🔥"}}
	reactions := newTestReactions(api)
	custom := &tg.ReactionCustomEmoji{DocumentID: 7}
	update := reactionsUpdate([]tg.MessagePeerReaction{peerReaction(ownerUser, custom, 1, false)}, 1)

	for range 2 {
		set := reactionSet(t)(reactions.messageReactions(context.Background(), tg.Entities{}, update))
		if len(set.Reactions) != 1 || set.Reactions[0].Emoji != "🔥" {
			t.Fatalf("reactions = %v, want 🔥", set.Reactions)
		}
	}
	if api.emojiHits != 1 {
		t.Fatalf("custom emoji looked up %d times, want once (cached)", api.emojiHits)
	}
}

func TestABotReactionReportsTheActorsNewestInItsTopic(t *testing.T) {
	t.Parallel()
	api := &fakeReactionAPI{messages: []tg.MessageClass{topicMessage(42, 12, 0)}}
	reactions := newTestReactions(api)
	update := &tg.UpdateBotMessageReaction{
		Peer:         &tg.PeerChannel{ChannelID: reactionChannel},
		MsgID:        42,
		Actor:        &tg.PeerUser{UserID: ownerUser},
		NewReactions: []tg.ReactionClass{emojiReaction("👍"), emojiReaction("❤️")},
	}

	for range 2 {
		frame, ok := reactions.botReaction(context.Background(), tg.Entities{}, update)
		if !ok {
			t.Fatal("bot reaction not reported")
		}
		event := frame.GetReaction()
		if event.Sender != "704338780" || event.Emoji != "❤️" || event.GetThreadId() != "12" || event.FromMe {
			t.Fatalf("event = %v", event)
		}
	}
	if api.fetches != 1 {
		t.Fatalf("message fetched %d times, want once (cached)", api.fetches)
	}

	update.NewReactions = nil
	frame, _ := reactions.botReaction(context.Background(), tg.Entities{}, update)
	if frame.GetReaction().Emoji != "" {
		t.Fatalf("a cleared reaction = %v, want an empty emoji", frame.GetReaction())
	}
}

func TestTheBotsOwnReactionIsNotReported(t *testing.T) {
	t.Parallel()
	reactions := newTestReactions(&fakeReactionAPI{})
	update := &tg.UpdateBotMessageReaction{
		Peer:         &tg.PeerChannel{ChannelID: reactionChannel},
		MsgID:        42,
		Actor:        &tg.PeerUser{UserID: bridgeUser},
		NewReactions: []tg.ReactionClass{emojiReaction("👍")},
	}
	if frame, ok := reactions.botReaction(context.Background(), tg.Entities{}, update); ok {
		t.Fatalf("reported the bridge's own reaction: %v", frame)
	}
}

func TestATopicsOpeningMessageIsInItsOwnTopic(t *testing.T) {
	t.Parallel()
	opening := &tg.MessageService{ID: 12, Action: &tg.MessageActionTopicCreate{Title: "Alice"}}
	if topic := messageTopic(opening, 12); topic != 12 {
		t.Fatalf("topic = %d, want 12", topic)
	}
	if topic := messageTopic(&tg.Message{ID: 5}, 5); topic != 0 {
		t.Fatalf("a message outside topics is in topic %d", topic)
	}
}

func TestBoundedCacheForgetsItsOldest(t *testing.T) {
	t.Parallel()
	cache := newBoundedCache[int, string](2)
	cache.put(1, "a")
	cache.put(2, "b")
	cache.put(1, "c")
	cache.put(3, "d")
	if _, ok := cache.get(1); ok {
		t.Fatal("the oldest entry should be forgotten")
	}
	if value, _ := cache.get(2); value != "b" {
		t.Fatalf("get(2) = %q", value)
	}
}
