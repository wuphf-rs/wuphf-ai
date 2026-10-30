package telegramconnector

import (
	"context"
	"errors"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/bin"
	"github.com/gotd/td/tg"
	"github.com/gotd/td/tgerr"
	"github.com/rs/zerolog"
)

// lookupInvoker answers a bot's peer lookups with known access hashes, and
// every send with an empty success, recording each request.
type lookupInvoker struct {
	mu       sync.Mutex
	requests []bin.Encoder
	channels map[int64]int64
	users    map[int64]int64
	// fail, when set, is returned for every lookup.
	fail error
}

func (l *lookupInvoker) Invoke(_ context.Context, input bin.Encoder, output bin.Decoder) error {
	l.mu.Lock()
	l.requests = append(l.requests, input)
	l.mu.Unlock()
	var response bin.Encoder
	switch request := input.(type) {
	case *tg.ChannelsGetChannelsRequest:
		if l.fail != nil {
			return l.fail
		}
		chats := &tg.MessagesChats{}
		for _, class := range request.ID {
			id := class.(*tg.InputChannel).ChannelID
			if hash, ok := l.channels[id]; ok {
				channel := &tg.Channel{ID: id, Photo: &tg.ChatPhotoEmpty{}}
				channel.SetAccessHash(hash)
				chats.Chats = append(chats.Chats, channel)
			}
		}
		response = chats
	case *tg.UsersGetUsersRequest:
		if l.fail != nil {
			return l.fail
		}
		users := &tg.UserClassVector{}
		for _, class := range request.ID {
			id := class.(*tg.InputUser).UserID
			if hash, ok := l.users[id]; ok {
				user := &tg.User{ID: id}
				user.SetAccessHash(hash)
				users.Elems = append(users.Elems, user)
			}
		}
		response = users
	case *tg.MessagesGetDialogsRequest:
		response = &tg.MessagesDialogs{}
	case *tg.UploadSaveFilePartRequest:
		response = &tg.BoolTrue{}
	default:
		response = &tg.UpdateShortSentMessage{ID: 1}
	}
	var buffer bin.Buffer
	if err := buffer.Encode(response); err != nil {
		return err
	}
	return output.Decode(&buffer)
}

// count is how many recorded requests have type T.
func count[T any](l *lookupInvoker) int {
	l.mu.Lock()
	defer l.mu.Unlock()
	n := 0
	for _, request := range l.requests {
		if _, ok := request.(T); ok {
			n++
		}
	}
	return n
}

func botCache(invoker *lookupInvoker) *PeerCache {
	cache := NewPeerCache()
	cache.UseBotAddressing(apiPeerResolver{api: tg.NewClient(invoker)}, zerolog.Nop())
	return cache
}

// Telegram accepts a zero access hash from a bot for text but not for media
// (CHANNEL_INVALID), so a bot looks the real hash up on a miss, once.
func TestBotMissLooksUpAndCachesTheChannelHash(t *testing.T) {
	t.Parallel()

	invoker := &lookupInvoker{channels: map[int64]int64{99: 777}}
	cache := botCache(invoker)
	for range 2 {
		peer, err := cache.InputPeer(context.Background(), "telegram:channel:99")
		if err != nil {
			t.Fatalf("InputPeer() error = %v", err)
		}
		if channel, ok := peer.(*tg.InputPeerChannel); !ok || channel.AccessHash != 777 {
			t.Fatalf("InputPeer() = %#v, want channel 99 with hash 777", peer)
		}
	}
	if got := count[*tg.ChannelsGetChannelsRequest](invoker); got != 1 {
		t.Fatalf("looked the channel up %d times, want once", got)
	}
}

func TestBotMissLooksUpTheUserHash(t *testing.T) {
	t.Parallel()

	invoker := &lookupInvoker{users: map[int64]int64{7: 555}}
	peer, err := botCache(invoker).InputPeer(context.Background(), "telegram:user:7")
	if err != nil {
		t.Fatalf("InputPeer() error = %v", err)
	}
	if user, ok := peer.(*tg.InputPeerUser); !ok || user.AccessHash != 555 {
		t.Fatalf("InputPeer() = %#v, want user 7 with hash 555", peer)
	}
}

// A failed lookup must not stop the bridge: text still goes out with the zero
// hash, as before lookups existed.
func TestBotMissFallsBackToAZeroHashWhenTheLookupFails(t *testing.T) {
	t.Parallel()

	invoker := &lookupInvoker{fail: tgerr.New(400, "CHANNEL_PRIVATE")}
	cache := botCache(invoker)
	peer, err := cache.InputPeer(context.Background(), "telegram:channel:99")
	if err != nil {
		t.Fatalf("InputPeer() error = %v, want a zero-hash peer", err)
	}
	if channel, ok := peer.(*tg.InputPeerChannel); !ok || channel.ChannelID != 99 || channel.AccessHash != 0 {
		t.Fatalf("InputPeer() = %#v, want channel 99 with a zero hash", peer)
	}
	// Telegram answering without the channel is a failed lookup too.
	empty := &lookupInvoker{}
	peer, err = botCache(empty).InputPeer(context.Background(), "telegram:channel:98")
	if err != nil {
		t.Fatalf("InputPeer() error = %v", err)
	}
	if channel, ok := peer.(*tg.InputPeerChannel); !ok || channel.AccessHash != 0 {
		t.Fatalf("InputPeer() = %#v, want a zero hash", peer)
	}
}

// countingResolver blocks every lookup until release closes, counting them.
type countingResolver struct {
	calls   atomic.Int32
	release chan struct{}
}

func (r *countingResolver) ChannelAccessHash(ctx context.Context, _ int64) (int64, error) {
	r.calls.Add(1)
	<-r.release
	return 42, nil
}

func (r *countingResolver) UserAccessHash(context.Context, int64) (int64, error) {
	return 0, errors.New("unused")
}

// Dispatches to a chat right after a restart arrive together; they share one
// lookup instead of each asking Telegram.
func TestConcurrentMissesShareOneLookup(t *testing.T) {
	t.Parallel()

	resolver := &countingResolver{release: make(chan struct{})}
	cache := NewPeerCache()
	cache.UseBotAddressing(resolver, zerolog.Nop())

	var started, done sync.WaitGroup
	hashes := make(chan int64, 8)
	for range 8 {
		started.Add(1)
		done.Add(1)
		go func() {
			defer done.Done()
			started.Done()
			peer, err := cache.InputPeer(context.Background(), "telegram:channel:99")
			if err != nil {
				t.Error(err)
				return
			}
			hashes <- peer.(*tg.InputPeerChannel).AccessHash
		}()
	}
	started.Wait()
	// Let every goroutine reach the lookup before it may finish.
	deadline := time.Now().Add(5 * time.Second)
	for resolver.calls.Load() == 0 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	time.Sleep(20 * time.Millisecond)
	close(resolver.release)
	done.Wait()
	close(hashes)
	for hash := range hashes {
		if hash != 42 {
			t.Fatalf("hash = %d, want 42", hash)
		}
	}
	if got := resolver.calls.Load(); got > 2 {
		t.Fatalf("%d lookups for one channel, want them shared", got)
	}
}

// photoBroker hands out a small image for every media id.
type photoBroker struct{ stubMediaBroker }

func (photoBroker) Download(context.Context, string) ([]byte, error) {
	return []byte("image bytes"), nil
}

// The failure seen live: a bot sending a photo into a chat it had not seen an
// update from used a zero hash and got CHANNEL_INVALID. The send now carries the
// looked-up hash.
func TestBotMediaUsesTheLookedUpHash(t *testing.T) {
	t.Parallel()

	invoker := &lookupInvoker{channels: map[int64]int64{2060340733: 31337}}
	api := tg.NewClient(invoker)
	peers := NewPeerCache()
	peers.UseBotAddressing(apiPeerResolver{api: api}, zerolog.Nop())
	sender := NewSender(api, peers, &photoBroker{}, nil, nil, true)

	mime := "image/jpeg"
	thread := "1037"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId:   "telegram:channel:2060340733",
		TargetThreadId: &thread,
		Attachments: []*wuphfpb.Attachment{{
			MediaId:  "m",
			Kind:     wuphfpb.Attachment_KIND_IMAGE,
			MimeType: &mime,
		}},
	})
	if err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	var media *tg.MessagesSendMediaRequest
	for _, request := range invoker.requests {
		if found, ok := request.(*tg.MessagesSendMediaRequest); ok {
			media = found
		}
	}
	if media == nil {
		t.Fatalf("no media was sent among %d requests", len(invoker.requests))
	}
	if channel, ok := media.Peer.(*tg.InputPeerChannel); !ok || channel.AccessHash != 31337 {
		t.Fatalf("media peer = %#v, want the looked-up hash 31337", media.Peer)
	}
}

// The stored session decides how peers are addressed, whatever the
// credentials say: a reused session file may hold the other kind.
func TestAddressingFollowsTheSessionKind(t *testing.T) {
	t.Parallel()

	invoker := &lookupInvoker{channels: map[int64]int64{99: 777}}
	api := tg.NewClient(invoker)

	bot := NewPeerCache()
	if err := prepareAddressing(context.Background(), &tg.User{ID: 1, Bot: true}, api, bot, zerolog.Nop()); err != nil {
		t.Fatalf("prepareAddressing(bot) error = %v", err)
	}
	if count[*tg.MessagesGetDialogsRequest](invoker) != 0 {
		t.Fatal("a bot session asked for its dialogs, which bots may not")
	}
	if peer, err := bot.InputPeer(context.Background(), "telegram:channel:99"); err != nil || peer.(*tg.InputPeerChannel).AccessHash != 777 {
		t.Fatalf("bot InputPeer() = %#v, %v, want the looked-up hash", peer, err)
	}

	user := NewPeerCache()
	if err := prepareAddressing(context.Background(), &tg.User{ID: 2}, api, user, zerolog.Nop()); err != nil {
		t.Fatalf("prepareAddressing(user) error = %v", err)
	}
	if count[*tg.MessagesGetDialogsRequest](invoker) != 1 {
		t.Fatal("a user session did not prime its peers from the dialog list")
	}
	if _, err := user.InputPeer(context.Background(), "telegram:channel:98"); err == nil {
		t.Fatal("a user session addressed an unseen channel")
	}
}

func TestSessionMismatchIsExplained(t *testing.T) {
	t.Parallel()

	bot := &tg.User{ID: 5140227513, Bot: true, Username: "PropheciTestBot"}
	user := &tg.User{ID: 704338780, Username: "someone"}

	if got := sessionMismatch("tg.session.json", true, bot); got != "" {
		t.Fatalf("bot token and bot session: %q, want no warning", got)
	}
	if got := sessionMismatch("tg.session.json", false, user); got != "" {
		t.Fatalf("no bot token and user session: %q, want no warning", got)
	}
	for _, test := range []struct {
		name        string
		hasBotToken bool
		self        *tg.User
		want        string
	}{
		{"user session with a bot token", true, user, "@someone"},
		{"bot session without a bot token", false, bot, "@PropheciTestBot"},
	} {
		got := sessionMismatch("tg.session.json", test.hasBotToken, test.self)
		if !strings.Contains(got, `"tg.session.json"`) || !strings.Contains(got, test.want) || !strings.Contains(got, "delete") {
			t.Fatalf("%s: warning %q should name the file, the account and the fix", test.name, got)
		}
	}
}
