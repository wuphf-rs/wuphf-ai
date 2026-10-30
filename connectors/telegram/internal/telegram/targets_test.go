package telegramconnector

import (
	"context"
	"testing"

	"github.com/gotd/td/tg"
	"github.com/rs/zerolog"
)

func TestFormatPeerRendersTypedChatID(t *testing.T) {
	t.Parallel()

	cases := map[string]tg.PeerClass{
		"telegram:user:88":    &tg.PeerUser{UserID: 88},
		"telegram:chat:7":     &tg.PeerChat{ChatID: 7},
		"telegram:channel:99": &tg.PeerChannel{ChannelID: 99},
	}
	for want, peer := range cases {
		if got := formatPeer(peer); got != want {
			t.Fatalf("formatPeer(%T) = %q, want %q", peer, got, want)
		}
	}
}

func TestPeerCacheResolvesObservedHashes(t *testing.T) {
	t.Parallel()

	cache := NewPeerCache()
	user := &tg.User{ID: 88}
	user.SetAccessHash(111)
	channel := &tg.Channel{ID: 99}
	channel.SetAccessHash(222)
	cache.Observe(tg.Entities{
		Users:    map[int64]*tg.User{88: user},
		Channels: map[int64]*tg.Channel{99: channel},
	})

	peer, err := cache.InputPeer(context.Background(), "telegram:user:88")
	if err != nil {
		t.Fatalf("InputPeer(user) error = %v", err)
	}
	if got, ok := peer.(*tg.InputPeerUser); !ok || got.AccessHash != 111 {
		t.Fatalf("InputPeer(user) = %#v", peer)
	}
	peer, err = cache.InputPeer(context.Background(), "telegram:channel:99")
	if err != nil {
		t.Fatalf("InputPeer(channel) error = %v", err)
	}
	if got, ok := peer.(*tg.InputPeerChannel); !ok || got.AccessHash != 222 {
		t.Fatalf("InputPeer(channel) = %#v", peer)
	}
}

func TestPeerCacheObserveClassesPrimesChannel(t *testing.T) {
	t.Parallel()

	cache := NewPeerCache()
	channel := &tg.Channel{ID: 99}
	channel.SetAccessHash(222)
	cache.ObserveClasses(nil, []tg.ChatClass{&tg.Chat{ID: 7}, channel})

	peer, err := cache.InputPeer(context.Background(), "telegram:channel:99")
	if err != nil {
		t.Fatalf("InputPeer(channel) error = %v", err)
	}
	if got, ok := peer.(*tg.InputPeerChannel); !ok || got.AccessHash != 222 {
		t.Fatalf("InputPeer(channel) = %#v", peer)
	}
}

func TestPeerCacheChatNeedsNoHash(t *testing.T) {
	t.Parallel()

	peer, err := NewPeerCache().InputPeer(context.Background(), "telegram:chat:7")
	if err != nil {
		t.Fatalf("InputPeer(chat) error = %v", err)
	}
	if got, ok := peer.(*tg.InputPeerChat); !ok || got.ChatID != 7 {
		t.Fatalf("InputPeer(chat) = %#v", peer)
	}
}

func TestPeerCacheUnknownChannelHashFails(t *testing.T) {
	t.Parallel()

	if _, err := NewPeerCache().InputPeer(context.Background(), "telegram:channel:99"); err == nil {
		t.Fatal("InputPeer() resolved channel without a known access hash")
	}
}

// A bot never sees an update for its own sends, so a target it has not observed
// has to stay addressable — otherwise the mirror stalls after every restart
// until somebody else posts in the chat.
func TestBotAddressingFallsBackToAZeroHashWithoutAResolver(t *testing.T) {
	t.Parallel()

	cache := NewPeerCache()
	cache.UseBotAddressing(nil, zerolog.Nop())

	peer, err := cache.InputPeer(context.Background(), "telegram:channel:99")
	if err != nil {
		t.Fatalf("InputPeer() error = %v, want a zero-hash peer", err)
	}
	channel, ok := peer.(*tg.InputPeerChannel)
	if !ok || channel.ChannelID != 99 || channel.AccessHash != 0 {
		t.Fatalf("InputPeer() = %#v, want channel 99 with a zero hash", peer)
	}

	user, err := cache.InputPeer(context.Background(), "telegram:user:7")
	if err != nil {
		t.Fatalf("InputPeer() error = %v, want a zero-hash peer", err)
	}
	if got, ok := user.(*tg.InputPeerUser); !ok || got.UserID != 7 || got.AccessHash != 0 {
		t.Fatalf("InputPeer() = %#v, want user 7 with a zero hash", user)
	}
}

// An observed hash is still preferred: bot addressing only decides what happens
// when the cache misses.
func TestBotAddressingPrefersAnObservedHash(t *testing.T) {
	t.Parallel()

	cache := NewPeerCache()
	cache.UseBotAddressing(nil, zerolog.Nop())
	channel := &tg.Channel{ID: 5}
	channel.SetAccessHash(4242)
	cache.Observe(tg.Entities{Channels: map[int64]*tg.Channel{5: channel}})

	peer, err := cache.InputPeer(context.Background(), "telegram:channel:5")
	if err != nil {
		t.Fatalf("InputPeer() error = %v", err)
	}
	if channel, ok := peer.(*tg.InputPeerChannel); !ok || channel.AccessHash != 4242 {
		t.Fatalf("InputPeer() = %#v, want the observed hash", peer)
	}
}
