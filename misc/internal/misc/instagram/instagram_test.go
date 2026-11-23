package instagram

import (
	"context"
	"encoding/json"
	"errors"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/functions"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/config"
	"github.com/wuphf-rs/wuphf-ai/misc/internal/misc"
)

// fakeInvoker answers instagram.fetch_media@1 per URL.
type fakeInvoker struct {
	mu      sync.Mutex
	results map[string]fakeResult
	calls   []string
}

type fakeResult struct {
	body     functions.InstagramFetchMediaResult
	mediaIDs []string
	err      error
}

func (f *fakeInvoker) Invoke(_ context.Context, name string, major uint32, args any, _ []string, result any) ([]string, error) {
	if name != functions.InstagramFetchMediaName || major != functions.InstagramFetchMediaMajor {
		return nil, errors.New("unexpected function")
	}
	url := args.(functions.InstagramFetchMediaArgs).URL
	f.mu.Lock()
	f.calls = append(f.calls, url)
	answer, ok := f.results[url]
	f.mu.Unlock()
	if !ok {
		return nil, errors.New("no such post")
	}
	if answer.err != nil {
		return nil, answer.err
	}
	body, _ := json.Marshal(answer.body)
	if err := json.Unmarshal(body, result); err != nil {
		return nil, err
	}
	return answer.mediaIDs, nil
}

type reply struct {
	to       string
	text     string
	mediaIDs []string
}

type fakeSender struct {
	mu      sync.Mutex
	replies []reply
	err     error
}

func (s *fakeSender) Reply(_ context.Context, to misc.Message, text string, mediaIDs []string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.err != nil {
		return s.err
	}
	s.replies = append(s.replies, reply{to: to.NativeID, text: text, mediaIDs: mediaIDs})
	return nil
}

const (
	group = "120363000000000000@g.us"
	reel  = "https://www.instagram.com/reel/Creel1/?igsh=abc"
	post  = "https://www.instagram.com/p/Cpost1/"
)

func newFeature(t *testing.T, cfg config.Instagram, invoker Invoker) *Feature {
	t.Helper()
	if cfg.Chats == nil {
		cfg.Chats = []string{"whatsapp-main:" + group, "telegram-main:-100123#42"}
	}
	feature, err := New(cfg, invoker, zerolog.Nop())
	if err != nil {
		t.Fatal(err)
	}
	return feature
}

func message(native, connector, chat, thread, text string) misc.Message {
	return misc.Message{ConnectorID: connector, ChatID: chat, ThreadID: thread, NativeID: native, Text: text, Direction: misc.Incoming}
}

func reelResult() fakeResult {
	return fakeResult{
		body: functions.InstagramFetchMediaResult{
			Items:   []functions.InstagramMediaItem{{Kind: "video"}},
			Author:  "someone",
			Caption: "a reel",
		},
		mediaIDs: []string{"media-reel"},
	}
}

func TestReelInAConfiguredChat(t *testing.T) {
	t.Parallel()
	invoker := &fakeInvoker{results: map[string]fakeResult{reel: reelResult()}}
	sender := &fakeSender{}
	feature := newFeature(t, config.Instagram{}, invoker)
	feature.Handle(context.Background(), message("m1", "whatsapp-main", group, "", "look "+reel), sender)
	if !slices.Equal(invoker.calls, []string{reel}) {
		t.Fatalf("calls = %v", invoker.calls)
	}
	want := []reply{{to: "m1", text: "@someone: a reel", mediaIDs: []string{"media-reel"}}}
	if !slices.EqualFunc(sender.replies, want, equalReply) {
		t.Fatalf("replies = %+v", sender.replies)
	}
}

func equalReply(a, b reply) bool {
	return a.to == b.to && a.text == b.text && slices.Equal(a.mediaIDs, b.mediaIDs)
}

func TestOnlyConfiguredChatsAndThreads(t *testing.T) {
	t.Parallel()
	invoker := &fakeInvoker{results: map[string]fakeResult{reel: reelResult()}}
	sender := &fakeSender{}
	feature := newFeature(t, config.Instagram{}, invoker)
	for _, m := range []misc.Message{
		message("a", "whatsapp-main", "other@g.us", "", reel),
		message("b", "whatsapp-alt", group, "", reel),
		message("c", "telegram-main", "-100123", "7", reel),
		message("d", "telegram-main", "-100123", "", reel),
		message("e", "whatsapp-main", group, "", "no link here"),
	} {
		feature.Handle(context.Background(), m, sender)
	}
	if len(invoker.calls) != 0 || len(sender.replies) != 0 {
		t.Fatalf("calls = %v, replies = %+v", invoker.calls, sender.replies)
	}
	// The configured thread, and any thread of a chat configured whole.
	feature.Handle(context.Background(), message("f", "telegram-main", "-100123", "42", reel), sender)
	feature.Handle(context.Background(), message("g", "whatsapp-main", group, "3", reel), sender)
	if len(sender.replies) != 2 {
		t.Fatalf("replies = %+v", sender.replies)
	}
}

func TestSameLinkTwiceIsAnsweredOnce(t *testing.T) {
	t.Parallel()
	invoker := &fakeInvoker{results: map[string]fakeResult{reel: reelResult()}}
	sender := &fakeSender{}
	feature := newFeature(t, config.Instagram{DedupHours: 1}, invoker)
	now := time.Unix(1_000_000, 0)
	feature.now = func() time.Time { return now }
	feature.Handle(context.Background(), message("m1", "whatsapp-main", group, "", reel), sender)
	// The same post by another link form, later, in the same chat.
	now = now.Add(30 * time.Minute)
	feature.Handle(context.Background(), message("m2", "whatsapp-main", group, "", "https://instagram.com/reels/Creel1"), sender)
	if len(invoker.calls) != 1 || len(sender.replies) != 1 {
		t.Fatalf("calls = %v, replies = %+v", invoker.calls, sender.replies)
	}
	// The same post in another chat is answered there.
	feature.Handle(context.Background(), message("m3", "telegram-main", "-100123", "42", reel), sender)
	// After the window, in the first chat again.
	now = now.Add(31 * time.Minute)
	feature.Handle(context.Background(), message("m4", "whatsapp-main", group, "", reel), sender)
	if len(sender.replies) != 3 || sender.replies[1].to != "m3" || sender.replies[2].to != "m4" {
		t.Fatalf("replies = %+v", sender.replies)
	}
}

func TestCarouselRepliesWithEveryMedium(t *testing.T) {
	t.Parallel()
	invoker := &fakeInvoker{results: map[string]fakeResult{post: {
		body: functions.InstagramFetchMediaResult{
			Items: []functions.InstagramMediaItem{{Kind: "image"}, {Kind: "video"}, {Kind: "image"}},
		},
		mediaIDs: []string{"m-1", "m-2", "m-3"},
	}}}
	sender := &fakeSender{}
	newFeature(t, config.Instagram{}, invoker).Handle(context.Background(), message("m1", "whatsapp-main", group, "", post), sender)
	if len(sender.replies) != 1 || !slices.Equal(sender.replies[0].mediaIDs, []string{"m-1", "m-2", "m-3"}) || sender.replies[0].text != "" {
		t.Fatalf("replies = %+v", sender.replies)
	}
}

func TestRateLimitSkipsTheEleventhFetchInAMinute(t *testing.T) {
	t.Parallel()
	results := map[string]fakeResult{}
	var text strings.Builder
	for index := range 11 {
		link := "https://www.instagram.com/p/C" + string(rune('a'+index)) + "/"
		results[link] = reelResult()
		text.WriteString(link + " ")
	}
	invoker := &fakeInvoker{results: results}
	sender := &fakeSender{}
	feature := newFeature(t, config.Instagram{}, invoker)
	now := time.Unix(1_000_000, 0)
	feature.now = func() time.Time { return now }
	feature.Handle(context.Background(), message("m1", "whatsapp-main", group, "", text.String()), sender)
	if len(invoker.calls) != 10 || len(sender.replies) != 10 {
		t.Fatalf("calls = %d, replies = %d; want 10 each", len(invoker.calls), len(sender.replies))
	}
	// A minute later the limit has room again.
	now = now.Add(time.Minute)
	feature.Handle(context.Background(), message("m2", "whatsapp-main", group, "", text.String()), sender)
	if len(invoker.calls) != 11 || invoker.calls[10] != "https://www.instagram.com/p/Ck/" {
		t.Fatalf("calls = %v", invoker.calls)
	}
}

func TestFailures(t *testing.T) {
	t.Parallel()
	results := map[string]fakeResult{
		reel: {err: errors.New("login required")},
		post: {body: functions.InstagramFetchMediaResult{}, mediaIDs: nil},
	}
	for _, replyOnFailure := range []bool{false, true} {
		invoker := &fakeInvoker{results: results}
		sender := &fakeSender{}
		feature := newFeature(t, config.Instagram{ReplyOnFailure: replyOnFailure}, invoker)
		feature.Handle(context.Background(), message("m1", "whatsapp-main", group, "", reel+" "+post), sender)
		want := 0
		if replyOnFailure {
			want = 2
		}
		if len(sender.replies) != want {
			t.Fatalf("reply_on_failure = %v: replies = %+v", replyOnFailure, sender.replies)
		}
		for _, r := range sender.replies {
			if r.text != FailureText || len(r.mediaIDs) != 0 {
				t.Fatalf("failure reply = %+v", r)
			}
		}
		// A failure is not remembered: the link is tried again.
		feature.Handle(context.Background(), message("m2", "whatsapp-main", group, "", reel), sender)
		if len(invoker.calls) != 3 {
			t.Fatalf("calls = %v", invoker.calls)
		}
	}
}

func TestAFailedReplyIsNotRemembered(t *testing.T) {
	t.Parallel()
	invoker := &fakeInvoker{results: map[string]fakeResult{reel: reelResult()}}
	sender := &fakeSender{err: errors.New("core unavailable")}
	feature := newFeature(t, config.Instagram{}, invoker)
	feature.Handle(context.Background(), message("m1", "whatsapp-main", group, "", reel), sender)
	sender.err = nil
	feature.Handle(context.Background(), message("m2", "whatsapp-main", group, "", reel), sender)
	if len(invoker.calls) != 2 || len(sender.replies) != 1 || sender.replies[0].to != "m2" {
		t.Fatalf("calls = %v, replies = %+v", invoker.calls, sender.replies)
	}
}

func TestCaptionSwitchAndLength(t *testing.T) {
	t.Parallel()
	off := false
	invoker := &fakeInvoker{results: map[string]fakeResult{reel: reelResult()}}
	sender := &fakeSender{}
	newFeature(t, config.Instagram{Caption: &off}, invoker).Handle(context.Background(), message("m1", "whatsapp-main", group, "", reel), sender)
	if len(sender.replies) != 1 || sender.replies[0].text != "" {
		t.Fatalf("caption = false: replies = %+v", sender.replies)
	}

	long := strings.Repeat("é", 250)
	caption := Caption("someone", long)
	if caption != "@someone: "+strings.Repeat("é", 200)+"…" {
		t.Fatalf("Caption(long) = %q", caption)
	}
	if got := Caption("someone", strings.Repeat("a", 200)); got != "@someone: "+strings.Repeat("a", 200) {
		t.Fatalf("Caption(200) = %q", got)
	}
	for _, test := range []struct{ author, text, want string }{
		{"", "just text", "just text"},
		{"someone", "  ", "@someone"},
		{"", "", ""},
		{"someone", "see https://www.instagram.com/p/Cother/", "@someone: see https://www.instagram.com/p/Cother/"},
	} {
		if got := Caption(test.author, test.text); got != test.want {
			t.Errorf("Caption(%q, %q) = %q, want %q", test.author, test.text, got, test.want)
		}
	}
}

func TestNewRejectsABadChat(t *testing.T) {
	t.Parallel()
	if _, err := New(config.Instagram{Enabled: true, Chats: []string{"no-colon"}}, &fakeInvoker{}, zerolog.Nop()); err == nil {
		t.Fatal("New accepted a chat that is not a reference")
	}
	if _, err := New(config.Instagram{Enabled: true}, &fakeInvoker{}, zerolog.Nop()); err == nil {
		t.Fatal("New accepted no chats")
	}
}

// blockingInvoker holds every call until release closes, counting how many
// run at once.
type blockingInvoker struct {
	mu      sync.Mutex
	running int
	most    int
	started chan struct{}
	release chan struct{}
}

func (b *blockingInvoker) Invoke(context.Context, string, uint32, any, []string, any) ([]string, error) {
	b.mu.Lock()
	b.running++
	b.most = max(b.most, b.running)
	b.mu.Unlock()
	b.started <- struct{}{}
	<-b.release
	b.mu.Lock()
	b.running--
	b.mu.Unlock()
	return []string{"m"}, nil
}

func TestAtMostMaxConcurrentFetches(t *testing.T) {
	t.Parallel()
	invoker := &blockingInvoker{started: make(chan struct{}, 8), release: make(chan struct{})}
	feature := newFeature(t, config.Instagram{MaxConcurrent: 2}, invoker)
	sender := &fakeSender{}
	var handlers sync.WaitGroup
	for index := range 4 {
		link := "https://www.instagram.com/p/C" + string(rune('a'+index)) + "/"
		handlers.Go(func() {
			feature.Handle(context.Background(), message(link, "whatsapp-main", group, "", link), sender)
		})
	}
	for range 2 {
		<-invoker.started
	}
	select {
	case <-invoker.started:
		t.Fatal("a third fetch started while two were running")
	case <-time.After(50 * time.Millisecond):
	}
	close(invoker.release)
	handlers.Wait()
	if invoker.most != 2 || len(sender.replies) != 4 {
		t.Fatalf("most at once = %d, replies = %d", invoker.most, len(sender.replies))
	}
}
