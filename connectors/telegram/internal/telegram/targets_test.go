package telegramconnector

import (
	"testing"

	"github.com/gotd/td/tg"
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

	peer, err := cache.InputPeer("telegram:user:88")
	if err != nil {
		t.Fatalf("InputPeer(user) error = %v", err)
	}
	if got, ok := peer.(*tg.InputPeerUser); !ok || got.AccessHash != 111 {
		t.Fatalf("InputPeer(user) = %#v", peer)
	}
	peer, err = cache.InputPeer("telegram:channel:99")
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

	peer, err := cache.InputPeer("telegram:channel:99")
	if err != nil {
		t.Fatalf("InputPeer(channel) error = %v", err)
	}
	if got, ok := peer.(*tg.InputPeerChannel); !ok || got.AccessHash != 222 {
		t.Fatalf("InputPeer(channel) = %#v", peer)
	}
}

func TestPeerCacheChatNeedsNoHash(t *testing.T) {
	t.Parallel()

	peer, err := NewPeerCache().InputPeer("telegram:chat:7")
	if err != nil {
		t.Fatalf("InputPeer(chat) error = %v", err)
	}
	if got, ok := peer.(*tg.InputPeerChat); !ok || got.ChatID != 7 {
		t.Fatalf("InputPeer(chat) = %#v", peer)
	}
}

func TestPeerCacheUnknownChannelHashFails(t *testing.T) {
	t.Parallel()

	if _, err := NewPeerCache().InputPeer("telegram:channel:99"); err == nil {
		t.Fatal("InputPeer() resolved channel without a known access hash")
	}
}
