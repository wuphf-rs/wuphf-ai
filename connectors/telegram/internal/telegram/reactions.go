package telegramconnector

import (
	"context"
	"strconv"
	"sync"
	"sync/atomic"

	"github.com/gotd/td/tg"
	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// reactionListLimit is how many reactions are fetched when an update does
// not list them all: far more than a mirror group sees on one message.
const reactionListLimit = 100

// reactionCacheLimit bounds the custom-emoji and topic caches.
const reactionCacheLimit = 1024

// reactionAPI is the part of the Telegram API reading reactions needs.
type reactionAPI interface {
	MessagesGetMessageReactionsList(ctx context.Context, request *tg.MessagesGetMessageReactionsListRequest) (*tg.MessagesMessageReactionsList, error)
	MessagesGetCustomEmojiDocuments(ctx context.Context, documentIDs []int64) ([]tg.DocumentClass, error)
	ChannelsGetMessages(ctx context.Context, request *tg.ChannelsGetMessagesRequest) (tg.MessagesMessagesClass, error)
}

// Reactions reports other accounts' reactions on messages (plan 080). The
// core decides whose reactions matter (the owner's); this only reads them.
// Reactions by this session's own account are never reported: those are the
// bridge's delivered and seen signals (👍, 👀).
//
// A user session gets the whole set of a message's reactions at once
// (UpdateMessageReactions), reported as a ReactionsEvent. A bot session gets
// one reactor's change (UpdateBotMessageReaction, only as a group admin),
// reported as a ReactionEvent.
type Reactions struct {
	// api is set before the client runs, so before any update arrives.
	api    reactionAPI
	peers  *PeerCache
	self   atomic.Int64
	emoji  *boundedCache[int64, string]
	topics *boundedCache[topicKey, int]
	logger zerolog.Logger
}

type topicKey struct {
	channel int64
	message int
}

func NewReactions(peers *PeerCache, logger zerolog.Logger) *Reactions {
	return &Reactions{
		peers:  peers,
		emoji:  newBoundedCache[int64, string](reactionCacheLimit),
		topics: newBoundedCache[topicKey, int](reactionCacheLimit),
		logger: logger.With().Str("component", "reactions").Logger(),
	}
}

// SetAPI gives the reader the client's API. Call it before the client runs.
func (r *Reactions) SetAPI(api reactionAPI) { r.api = api }

// SetSelf records this session's own user id once it is known.
func (r *Reactions) SetSelf(id int64) { r.self.Store(id) }

// messageReactions turns a user session's update into the message's complete
// reaction set. It reports nothing when the set cannot be known in full: a
// partial set would read as everyone missing from it having cleared theirs.
func (r *Reactions) messageReactions(ctx context.Context, entities tg.Entities, update *tg.UpdateMessageReactions) (*wuphfpb.ClientFrame, bool) {
	chatID := formatPeer(update.Peer)
	reactions := update.Reactions
	r.logger.Debug().Str("chat", chatID).Int("message", update.MsgID).
		Bool("min", reactions.Min).Int("results", len(reactions.Results)).
		Int("recent", len(reactions.RecentReactions)).Msg("reaction update")
	listed, names := reactions.RecentReactions, entities
	if reactions.Min || len(listed) < reactionCount(reactions.Results) {
		var ok bool
		listed, names, ok = r.list(ctx, chatID, update.MsgID)
		if !ok {
			return nil, false
		}
	}
	event := &wuphfpb.ReactionsEvent{
		ChatId:            chatID,
		PlatformMessageId: strconv.Itoa(update.MsgID),
		Reactions:         r.newestByReactor(ctx, listed, names),
	}
	if topic, ok := update.GetTopMsgID(); ok && topic != 0 {
		thread := strconv.Itoa(topic)
		event.ThreadId = &thread
	}
	return &wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_ReactionsSet{ReactionsSet: event}}, true
}

// botReaction turns a bot session's update into the actor's new reaction:
// their newest one, or none when they cleared it.
func (r *Reactions) botReaction(ctx context.Context, entities tg.Entities, update *tg.UpdateBotMessageReaction) (*wuphfpb.ClientFrame, bool) {
	chatID := formatPeer(update.Peer)
	r.logger.Debug().Str("chat", chatID).Int("message", update.MsgID).
		Str("actor", formatPeer(update.Actor)).Int("reactions", len(update.NewReactions)).
		Msg("reaction update")
	if r.isSelf(update.Actor) {
		return nil, false
	}
	event := &wuphfpb.ReactionEvent{
		ChatId:            chatID,
		PlatformMessageId: strconv.Itoa(update.MsgID),
		Sender:            reactorID(update.Actor),
	}
	if name := chatTitle(entities, update.Actor); name != "" {
		event.SenderName = &name
	}
	// Telegram appends a newly chosen reaction, so the newest is last.
	for i := len(update.NewReactions) - 1; i >= 0; i-- {
		if emoji, ok := r.emojiOf(ctx, update.NewReactions[i]); ok {
			event.Emoji = emoji
			break
		}
	}
	if topic, ok := r.topic(ctx, update.Peer, update.MsgID); ok {
		thread := strconv.Itoa(topic)
		event.ThreadId = &thread
	}
	return &wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_Reaction{Reaction: event}}, true
}

// list fetches a message's reactions when the update does not carry them all.
func (r *Reactions) list(ctx context.Context, chatID string, messageID int) ([]tg.MessagePeerReaction, tg.Entities, bool) {
	peer, err := r.peers.InputPeer(ctx, chatID)
	if err != nil {
		r.logger.Debug().Err(err).Str("chat", chatID).Msg("reactions not reported: chat unknown")
		return nil, tg.Entities{}, false
	}
	result, err := r.api.MessagesGetMessageReactionsList(ctx, &tg.MessagesGetMessageReactionsListRequest{
		Peer:  peer,
		ID:    messageID,
		Limit: reactionListLimit,
	})
	if err != nil {
		r.logger.Debug().Err(err).Str("chat", chatID).Int("message", messageID).Msg("reactions not reported: the list could not be read")
		return nil, tg.Entities{}, false
	}
	if result.NextOffset != "" {
		r.logger.Debug().Str("chat", chatID).Int("message", messageID).Msg("reactions not reported: more than one page")
		return nil, tg.Entities{}, false
	}
	r.peers.ObserveClasses(result.Users, result.Chats)
	names := tg.Entities{Users: map[int64]*tg.User{}, Chats: map[int64]*tg.Chat{}, Channels: map[int64]*tg.Channel{}}
	for _, user := range result.Users {
		if user, ok := user.(*tg.User); ok {
			names.Users[user.ID] = user
		}
	}
	return result.Reactions, names, true
}

// newestByReactor keeps each reactor's newest reaction, leaving out this
// account's own and reactions with no emoji (a paid ⭐).
func (r *Reactions) newestByReactor(ctx context.Context, listed []tg.MessagePeerReaction, names tg.Entities) []*wuphfpb.ReactorReaction {
	type newest struct {
		date     int
		reaction *wuphfpb.ReactorReaction
	}
	byReactor := map[string]newest{}
	var order []string
	for _, entry := range listed {
		if entry.My || r.isSelf(entry.PeerID) {
			continue
		}
		emoji, ok := r.emojiOf(ctx, entry.Reaction)
		if !ok {
			continue
		}
		reactor := reactorID(entry.PeerID)
		current, seen := byReactor[reactor]
		if seen && current.date > entry.Date {
			continue
		}
		reaction := &wuphfpb.ReactorReaction{Sender: reactor, Emoji: emoji}
		if name := chatTitle(names, entry.PeerID); name != "" {
			reaction.SenderName = &name
		}
		if !seen {
			order = append(order, reactor)
		}
		byReactor[reactor] = newest{date: entry.Date, reaction: reaction}
	}
	reactions := make([]*wuphfpb.ReactorReaction, 0, len(order))
	for _, reactor := range order {
		reactions = append(reactions, byReactor[reactor].reaction)
	}
	return reactions
}

// emojiOf is the emoji a reaction shows: a custom emoji as the standard one
// it stands for, and none for a paid reaction.
func (r *Reactions) emojiOf(ctx context.Context, reaction tg.ReactionClass) (string, bool) {
	switch reaction := reaction.(type) {
	case *tg.ReactionEmoji:
		return reaction.Emoticon, reaction.Emoticon != ""
	case *tg.ReactionCustomEmoji:
		if alt, ok := r.emoji.get(reaction.DocumentID); ok {
			return alt, true
		}
		documents, err := r.api.MessagesGetCustomEmojiDocuments(ctx, []int64{reaction.DocumentID})
		if err != nil {
			r.logger.Debug().Err(err).Int64("document", reaction.DocumentID).Msg("custom emoji reaction skipped")
			return "", false
		}
		alt := customEmojiAlt(documents)
		if alt == "" {
			return "", false
		}
		r.emoji.put(reaction.DocumentID, alt)
		return alt, true
	default:
		return "", false
	}
}

func customEmojiAlt(documents []tg.DocumentClass) string {
	for _, document := range documents {
		document, ok := document.(*tg.Document)
		if !ok {
			continue
		}
		for _, attribute := range document.Attributes {
			if emoji, ok := attribute.(*tg.DocumentAttributeCustomEmoji); ok && emoji.Alt != "" {
				return emoji.Alt
			}
		}
	}
	return ""
}

// topic is the forum topic a channel message is in, which a bot's reaction
// update does not say: read from the message, and remembered.
func (r *Reactions) topic(ctx context.Context, peer tg.PeerClass, messageID int) (int, bool) {
	channel, ok := peer.(*tg.PeerChannel)
	if !ok {
		return 0, false
	}
	key := topicKey{channel: channel.ChannelID, message: messageID}
	if topic, ok := r.topics.get(key); ok {
		return topic, topic != 0
	}
	input, err := r.peers.InputPeer(ctx, formatPeer(peer))
	if err != nil {
		return 0, false
	}
	inputChannel, ok := input.(*tg.InputPeerChannel)
	if !ok {
		return 0, false
	}
	result, err := r.api.ChannelsGetMessages(ctx, &tg.ChannelsGetMessagesRequest{
		Channel: &tg.InputChannel{ChannelID: inputChannel.ChannelID, AccessHash: inputChannel.AccessHash},
		ID:      []tg.InputMessageClass{&tg.InputMessageID{ID: messageID}},
	})
	if err != nil {
		r.logger.Debug().Err(err).Int("message", messageID).Msg("reacted message's topic unknown")
		return 0, false
	}
	topic := 0
	if messages, ok := result.(interface{ GetMessages() []tg.MessageClass }); ok {
		for _, message := range messages.GetMessages() {
			topic = messageTopic(message, messageID)
		}
	}
	r.topics.put(key, topic)
	return topic, topic != 0
}

// messageTopic is the topic a message is in: the topic it replies into, or
// itself when it is a topic's opening message. 0 is none.
func messageTopic(class tg.MessageClass, messageID int) int {
	switch message := class.(type) {
	case *tg.Message:
		if topic, ok := topicRootID(message); ok {
			return topic
		}
	case *tg.MessageService:
		if _, ok := message.Action.(*tg.MessageActionTopicCreate); ok {
			return messageID
		}
		if reply, ok := message.GetReplyTo(); ok {
			if topic, ok := topicOf(reply); ok {
				return topic
			}
		}
	}
	return 0
}

func (r *Reactions) isSelf(peer tg.PeerClass) bool {
	user, ok := peer.(*tg.PeerUser)
	return ok && user.UserID != 0 && user.UserID == r.self.Load()
}

// reactorID names a reactor as owner_ids name Telegram users: the plain user
// id. A chat reacting (a channel posting as itself) keeps its chat id.
func reactorID(peer tg.PeerClass) string {
	if user, ok := peer.(*tg.PeerUser); ok {
		return strconv.FormatInt(user.UserID, 10)
	}
	return formatPeer(peer)
}

// reactionCount is how many reactions the counters say a message has.
func reactionCount(results []tg.ReactionCount) int {
	total := 0
	for _, result := range results {
		total += result.Count
	}
	return total
}

// boundedCache is a map that forgets its oldest entry beyond its limit.
type boundedCache[K comparable, V any] struct {
	mu     sync.Mutex
	limit  int
	order  []K
	values map[K]V
}

func newBoundedCache[K comparable, V any](limit int) *boundedCache[K, V] {
	return &boundedCache[K, V]{limit: limit, values: make(map[K]V)}
}

func (c *boundedCache[K, V]) get(key K) (V, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	value, ok := c.values[key]
	return value, ok
}

func (c *boundedCache[K, V]) put(key K, value V) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if _, ok := c.values[key]; !ok {
		c.order = append(c.order, key)
		if len(c.order) > c.limit {
			delete(c.values, c.order[0])
			c.order = c.order[1:]
		}
	}
	c.values[key] = value
}
