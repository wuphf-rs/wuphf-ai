package telegramconnector

import (
	"fmt"
	"strconv"
	"strings"
	"sync"

	coreclient "github.com/akshettrj/wuphf/connectors/telegram/internal/core"
	"github.com/gotd/td/tg"
)

// PeerCache records the access hashes Telegram requires to address a peer for
// outbound sends. The core deals only in opaque chat ids (see formatPeer); the
// connector resolves them back to InputPeers using hashes observed on inbound
// updates. Users and channels need a hash; basic chats are addressable by id.
type PeerCache struct {
	mu       sync.RWMutex
	users    map[int64]int64
	channels map[int64]int64
}

func NewPeerCache() *PeerCache {
	return &PeerCache{
		users:    make(map[int64]int64),
		channels: make(map[int64]int64),
	}
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
func (c *PeerCache) InputPeer(chatID string) (tg.InputPeerClass, error) {
	kind, id, err := parseChatID(chatID)
	if err != nil {
		return nil, coreclient.Permanent(err)
	}
	c.mu.RLock()
	defer c.mu.RUnlock()
	switch kind {
	case "user":
		hash, ok := c.users[id]
		if !ok {
			return nil, fmt.Errorf("no known access hash for Telegram user %d", id)
		}
		return &tg.InputPeerUser{UserID: id, AccessHash: hash}, nil
	case "chat":
		return &tg.InputPeerChat{ChatID: id}, nil
	case "channel":
		hash, ok := c.channels[id]
		if !ok {
			return nil, fmt.Errorf("no known access hash for Telegram channel %d", id)
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
