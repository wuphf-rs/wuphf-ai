package telegramconnector

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"sync"

	"github.com/gotd/td/tg"
	"github.com/rs/zerolog"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"golang.org/x/sync/singleflight"
)

// PeerCache records the access hashes Telegram requires to address a peer for
// outbound sends. The core deals only in opaque chat ids (see formatPeer); the
// connector resolves them back to InputPeers using hashes observed on inbound
// updates. Users and channels need a hash; basic chats are addressable by id.
type PeerCache struct {
	mu       sync.RWMutex
	users    map[int64]int64
	channels map[int64]int64
	bot      bool
	resolver PeerResolver
	logger   zerolog.Logger
	// lookups collapses concurrent misses for one peer into a single call.
	lookups singleflight.Group
}

// PeerResolver looks up the access hash of a peer a bot has not seen an update
// from. Telegram lets a bot ask for a peer it has access to by id alone.
type PeerResolver interface {
	ChannelAccessHash(ctx context.Context, id int64) (int64, error)
	UserAccessHash(ctx context.Context, id int64) (int64, error)
}

func NewPeerCache() *PeerCache {
	return &PeerCache{
		users:    make(map[int64]int64),
		channels: make(map[int64]int64),
		logger:   zerolog.Nop(),
	}
}

// UseBotAddressing makes peers this session has never seen an update from
// addressable, for a bot session.
//
// A user session primes its hashes from the dialog list at start-up, but that
// call is user-only, so a bot knows a hash only once an update carries one. A
// bot receives no update for its own sends, so without this the mirror stalls
// after every restart until somebody else happens to post in the target chat —
// and a quiet chat would stall forever.
//
// On a miss the cache asks resolver for the real hash and records it. Telegram
// accepts a zero hash from a bot for sending text, but not for sending media
// (CHANNEL_INVALID), so the zero hash is only the fallback when resolver is nil
// or the lookup fails. The cache still records real hashes when updates carry
// them; this only decides what happens on a miss.
func (c *PeerCache) UseBotAddressing(resolver PeerResolver, logger zerolog.Logger) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.bot = true
	c.resolver = resolver
	c.logger = logger
}

// Observe records the access hashes carried by an update's entities so later
// dispatches to those peers can be addressed.
func (c *PeerCache) Observe(entities tg.Entities) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for id, user := range entities.Users {
		if hash, ok := user.GetAccessHash(); ok {
			c.users[id] = hash
		}
	}
	for id, channel := range entities.Channels {
		if hash, ok := channel.GetAccessHash(); ok {
			c.channels[id] = hash
		}
	}
}

// ObserveClasses records access hashes from a bare user/chat list, as returned
// by dialog and contact queries that prime the cache before any update arrives.
func (c *PeerCache) ObserveClasses(users []tg.UserClass, chats []tg.ChatClass) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, candidate := range users {
		if user, ok := candidate.(*tg.User); ok {
			if hash, ok := user.GetAccessHash(); ok {
				c.users[user.ID] = hash
			}
		}
	}
	for _, candidate := range chats {
		if channel, ok := candidate.(*tg.Channel); ok {
			if hash, ok := channel.GetAccessHash(); ok {
				c.channels[channel.ID] = hash
			}
		}
	}
}

// InputPeer resolves an opaque chat id to a Telegram input peer.
func (c *PeerCache) InputPeer(ctx context.Context, chatID string) (tg.InputPeerClass, error) {
	kind, id, err := parseChatID(chatID)
	if err != nil {
		return nil, coreclient.Permanent(err)
	}
	switch kind {
	case "user":
		hash, err := c.accessHash(ctx, kind, id)
		if err != nil {
			return nil, err
		}
		return &tg.InputPeerUser{UserID: id, AccessHash: hash}, nil
	case "chat":
		return &tg.InputPeerChat{ChatID: id}, nil
	case "channel":
		hash, err := c.accessHash(ctx, kind, id)
		if err != nil {
			return nil, err
		}
		return &tg.InputPeerChannel{ChannelID: id, AccessHash: hash}, nil
	default:
		return nil, coreclient.Permanent(fmt.Errorf("unsupported Telegram chat id %q", chatID))
	}
}

// formatPeer renders a Telegram peer as an opaque, self-describing chat id.
func formatPeer(peer tg.PeerClass) string {
	switch value := peer.(type) {
	case *tg.PeerUser:
		return fmt.Sprintf("telegram:user:%d", value.UserID)
	case *tg.PeerChat:
		return fmt.Sprintf("telegram:chat:%d", value.ChatID)
	case *tg.PeerChannel:
		return fmt.Sprintf("telegram:channel:%d", value.ChannelID)
	default:
		return "telegram:unknown"
	}
}

// chatTitle derives a human-readable title for a peer from an update's
// entities, used so the core can label a freshly provisioned mirror thread.
func chatTitle(entities tg.Entities, peer tg.PeerClass) string {
	switch value := peer.(type) {
	case *tg.PeerUser:
		if user, ok := entities.Users[value.UserID]; ok {
			name := strings.TrimSpace(user.FirstName + " " + user.LastName)
			if name != "" {
				return name
			}
			if username, ok := user.GetUsername(); ok {
				return username
			}
		}
	case *tg.PeerChat:
		if chat, ok := entities.Chats[value.ChatID]; ok {
			return chat.Title
		}
	case *tg.PeerChannel:
		if channel, ok := entities.Channels[value.ChannelID]; ok {
			return channel.Title
		}
	}
	return ""
}

func parseChatID(chatID string) (kind string, id int64, err error) {
	parts := strings.Split(chatID, ":")
	if len(parts) != 3 || parts[0] != "telegram" {
		return "", 0, fmt.Errorf("malformed Telegram chat id %q", chatID)
	}
	id, err = strconv.ParseInt(parts[2], 10, 64)
	if err != nil {
		return "", 0, fmt.Errorf("invalid Telegram chat id %q: %w", chatID, err)
	}
	return parts[1], id, nil
}

// accessHash returns the recorded hash for a user or channel. On a miss, a bot
// session looks the hash up and records it, falling back to zero when it
// cannot (see UseBotAddressing). For a user session the error is deliberately
// not permanent: a session that has simply not observed the peer yet may still
// learn it, so the dispatch is worth retrying rather than dead-lettering.
func (c *PeerCache) accessHash(ctx context.Context, kind string, id int64) (int64, error) {
	c.mu.RLock()
	known := c.channels
	if kind == "user" {
		known = c.users
	}
	hash, ok := known[id]
	bot, resolver, logger := c.bot, c.resolver, c.logger
	c.mu.RUnlock()
	if ok {
		return hash, nil
	}
	if !bot {
		return 0, fmt.Errorf("no known access hash for Telegram %s %d", kind, id)
	}
	if resolver == nil {
		return 0, nil
	}
	key := kind + ":" + strconv.FormatInt(id, 10)
	value, err, _ := c.lookups.Do(key, func() (any, error) {
		return c.lookup(ctx, resolver, kind, id)
	})
	if err != nil {
		logger.Debug().Err(err).Str("peer", key).Msg("cannot look up a Telegram access hash; addressing it with a zero hash")
		return 0, nil
	}
	return value.(int64), nil
}

// lookup asks resolver for a peer's hash and records it.
func (c *PeerCache) lookup(ctx context.Context, resolver PeerResolver, kind string, id int64) (int64, error) {
	var hash int64
	var err error
	if kind == "user" {
		hash, err = resolver.UserAccessHash(ctx, id)
	} else {
		hash, err = resolver.ChannelAccessHash(ctx, id)
	}
	if err != nil {
		return 0, err
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if kind == "user" {
		c.users[id] = hash
	} else {
		c.channels[id] = hash
	}
	return hash, nil
}

// apiPeerResolver looks hashes up through a bot session's API: Telegram
// returns the full channel or user to a bot that asks by id with a zero hash,
// as long as the bot has access to it.
type apiPeerResolver struct {
	api *tg.Client
}

func (r apiPeerResolver) ChannelAccessHash(ctx context.Context, id int64) (int64, error) {
	chats, err := r.api.ChannelsGetChannels(ctx, []tg.InputChannelClass{&tg.InputChannel{ChannelID: id}})
	if err != nil {
		return 0, fmt.Errorf("look up Telegram channel %d: %w", id, err)
	}
	for _, chat := range chats.GetChats() {
		if channel, ok := chat.(*tg.Channel); ok && channel.ID == id {
			if hash, ok := channel.GetAccessHash(); ok {
				return hash, nil
			}
		}
	}
	return 0, fmt.Errorf("look up Telegram channel %d: %w", id, errNoAccessHash)
}

func (r apiPeerResolver) UserAccessHash(ctx context.Context, id int64) (int64, error) {
	users, err := r.api.UsersGetUsers(ctx, []tg.InputUserClass{&tg.InputUser{UserID: id}})
	if err != nil {
		return 0, fmt.Errorf("look up Telegram user %d: %w", id, err)
	}
	for _, class := range users {
		if user, ok := class.(*tg.User); ok && user.ID == id {
			if hash, ok := user.GetAccessHash(); ok {
				return hash, nil
			}
		}
	}
	return 0, fmt.Errorf("look up Telegram user %d: %w", id, errNoAccessHash)
}

// errNoAccessHash means Telegram answered a lookup without the peer's hash
// (e.g. a channel the bot has no access to).
var errNoAccessHash = errors.New("no access hash in the answer")
