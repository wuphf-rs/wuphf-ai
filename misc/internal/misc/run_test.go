package misc

import (
	"context"
	"errors"
	"io"
	"slices"
	"sync"
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

const patience = 5 * time.Second

// fakeStream replays its messages, then ends with end.
type fakeStream struct {
	grpc.ClientStream
	messages []*wuphfpb.ObservedMessage
	end      error
}

func (s *fakeStream) Recv() (*wuphfpb.ObservedMessage, error) {
	if len(s.messages) == 0 {
		return nil, s.end
	}
	next := s.messages[0]
	s.messages = s.messages[1:]
	return next, nil
}

// fakeObserver serves one stream per subscription, then blocks until the
// caller's context ends; it records every send.
type fakeObserver struct {
	mu            sync.Mutex
	streams       [][]*wuphfpb.ObservedMessage
	end           error
	subscriptions int
	sent          []*wuphfpb.SendMessageRequest
	sentID        string
}

func (f *fakeObserver) SubscribeMessages(ctx context.Context, _ *wuphfpb.SubscribeMessagesRequest, _ ...grpc.CallOption) (grpc.ServerStreamingClient[wuphfpb.ObservedMessage], error) {
	f.mu.Lock()
	f.subscriptions++
	if len(f.streams) == 0 {
		f.mu.Unlock()
		<-ctx.Done()
		return nil, ctx.Err()
	}
	messages := f.streams[0]
	f.streams = f.streams[1:]
	f.mu.Unlock()
	end := f.end
	if end == nil {
		end = io.EOF
	}
	return &fakeStream{messages: messages, end: end}, nil
}

func (f *fakeObserver) SendMessage(_ context.Context, request *wuphfpb.SendMessageRequest, _ ...grpc.CallOption) (*wuphfpb.SendMessageResponse, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.sent = append(f.sent, request)
	return &wuphfpb.SendMessageResponse{PlatformMessageId: f.sentID}, nil
}

// recordingFeature records the native id of every message it is handed and
// replies when the message text says so.
type recordingFeature struct {
	name    string
	handled chan Message
}

func (f *recordingFeature) Name() string { return f.name }

func (f *recordingFeature) Handle(ctx context.Context, message Message, out Sender) {
	if message.Text == "reply please" {
		_ = out.Reply(ctx, message, "here you go", []string{"media-1"})
	}
	f.handled <- message
}

func newFeature(name string) *recordingFeature {
	return &recordingFeature{name: name, handled: make(chan Message, 32)}
}

func observed(native string, direction wuphfpb.ObservedDirection, text string) *wuphfpb.ObservedMessage {
	return &wuphfpb.ObservedMessage{
		ConnectorId:       "whatsapp-main",
		ChatId:            "1@g.us",
		PlatformMessageId: native,
		Sender:            "someone",
		Text:              &text,
		Direction:         direction,
	}
}

const (
	incoming = wuphfpb.ObservedDirection_OBSERVED_DIRECTION_INCOMING
	outgoing = wuphfpb.ObservedDirection_OBSERVED_DIRECTION_OUTGOING
)

func withSentBy(message *wuphfpb.ObservedMessage, client string) *wuphfpb.ObservedMessage {
	message.SentBy = &client
	return message
}

// runUntil runs the runner until want messages reached feature, then stops it.
func runUntil(t *testing.T, observer *fakeObserver, feature *recordingFeature, want int) []string {
	t.Helper()
	runner := NewRunner(observer, []Feature{feature}, time.Millisecond, zerolog.Nop())
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- runner.Run(ctx) }()
	var got []string
	for range want {
		select {
		case message := <-feature.handled:
			got = append(got, message.NativeID)
		case <-time.After(patience):
			t.Fatalf("handled %v; timed out waiting for %d messages", got, want)
		}
	}
	// Nothing else arrives once the streams are drained.
	select {
	case message := <-feature.handled:
		t.Fatalf("handled an extra message %q", message.NativeID)
	case <-time.After(50 * time.Millisecond):
	}
	cancel()
	if err := <-done; !errors.Is(err, context.Canceled) {
		t.Fatalf("Run() = %v, want context.Canceled", err)
	}
	slices.Sort(got)
	return got
}

func TestRunnerFilter(t *testing.T) {
	t.Parallel()
	edited := observed("edited", incoming, "x")
	edited.Edited = true
	deleted := observed("deleted", incoming, "x")
	deleted.Deleted = true
	observer := &fakeObserver{streams: [][]*wuphfpb.ObservedMessage{{
		observed("in", incoming, "from a contact or the owner's phone"),
		withSentBy(observed("tui", outgoing, "typed in the TUI"), "wuphf-tui"),
		observed("plain-out", outgoing, "an owner send with no client"),
		withSentBy(observed("own", outgoing, "a reply of wuphf-misc"), ClientName),
		// The platform's echo of wuphf-misc's reply re-streams its row.
		withSentBy(observed("own", outgoing, "a reply of wuphf-misc"), ClientName),
		edited,
		deleted,
		observed("unknown", wuphfpb.ObservedDirection_OBSERVED_DIRECTION_UNSPECIFIED, "?"),
		// The TUI send's echo: the same row again.
		withSentBy(observed("tui", outgoing, "typed in the TUI"), "wuphf-tui"),
	}}}
	got := runUntil(t, observer, newFeature("f"), 3)
	if !slices.Equal(got, []string{"in", "plain-out", "tui"}) {
		t.Fatalf("handled %v", got)
	}
}

func TestRunnerDeliversToEveryFeatureAndResubscribes(t *testing.T) {
	t.Parallel()
	observer := &fakeObserver{streams: [][]*wuphfpb.ObservedMessage{
		{observed("a", incoming, "one")},
		{observed("b", incoming, "two")},
	}}
	first, second := newFeature("first"), newFeature("second")
	runner := NewRunner(observer, []Feature{first, second}, time.Millisecond, zerolog.Nop())
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- runner.Run(ctx) }()
	for _, feature := range []*recordingFeature{first, second} {
		var got []string
		for range 2 {
			select {
			case message := <-feature.handled:
				got = append(got, message.NativeID)
			case <-time.After(patience):
				t.Fatalf("%s handled %v; timed out", feature.name, got)
			}
		}
		slices.Sort(got)
		if !slices.Equal(got, []string{"a", "b"}) {
			t.Fatalf("%s handled %v", feature.name, got)
		}
	}
	cancel()
	<-done
	observer.mu.Lock()
	defer observer.mu.Unlock()
	if observer.subscriptions < 2 {
		t.Fatalf("subscriptions = %d; the runner must resubscribe after each stream ends", observer.subscriptions)
	}
}

func TestRunnerStopsOnARejectedToken(t *testing.T) {
	t.Parallel()
	observer := &fakeObserver{
		streams: [][]*wuphfpb.ObservedMessage{{}},
		end:     status.Error(codes.Unauthenticated, "bad token"),
	}
	runner := NewRunner(observer, []Feature{newFeature("f")}, time.Millisecond, zerolog.Nop())
	ctx, cancel := context.WithTimeout(context.Background(), patience)
	defer cancel()
	if err := runner.Run(ctx); status.Code(errors.Unwrap(err)) != codes.Unauthenticated {
		t.Fatalf("Run() = %v, want the rejection", err)
	}
}

func TestReplyAddressesTheMessageAndSkipsItsEcho(t *testing.T) {
	t.Parallel()
	thread := "topic-7"
	trigger := observed("link", incoming, "reply please")
	trigger.ThreadId = &thread
	// The platform reports the reply back before the core has its
	// acknowledgement: incoming, no sent_by, same text, same chat.
	early := observed("reply-echo", incoming, "here you go")
	early.ThreadId = &thread
	// Then the settled row, under the native id SendMessage returned.
	settled := withSentBy(observed("reply-1", outgoing, "here you go"), ClientName)
	settled.ThreadId = &thread
	observer := &fakeObserver{sentID: "reply-1"}
	feature := newFeature("f")
	runner := NewRunner(observer, []Feature{feature}, time.Millisecond, zerolog.Nop())

	// The trigger first, alone, so the reply is sent before the echoes arrive.
	observer.streams = [][]*wuphfpb.ObservedMessage{{trigger}}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- runner.Run(ctx) }()
	select {
	case <-feature.handled:
	case <-time.After(patience):
		t.Fatal("trigger not handled")
	}
	cancel()
	<-done

	observer.mu.Lock()
	if len(observer.sent) != 1 {
		observer.mu.Unlock()
		t.Fatalf("sent %d replies", len(observer.sent))
	}
	request := observer.sent[0]
	observer.mu.Unlock()
	if request.GetConnectorId() != "whatsapp-main" || request.GetChatId() != "1@g.us" || request.GetThreadId() != thread ||
		request.GetReplyToPlatformMessageId() != "link" || request.GetText() != "here you go" ||
		!slices.Equal(request.GetMediaIds(), []string{"media-1"}) || request.GetClient() != ClientName {
		t.Fatalf("reply request = %v", request)
	}

	// The same runner sees both echoes: neither reaches the feature.
	observer.streams = [][]*wuphfpb.ObservedMessage{{early, settled, observed("next", incoming, "unrelated")}}
	ctx, cancel = context.WithCancel(context.Background())
	go func() { done <- runner.Run(ctx) }()
	select {
	case message := <-feature.handled:
		if message.NativeID != "next" {
			t.Fatalf("handled %q; an echo of the reply reached the feature", message.NativeID)
		}
	case <-time.After(patience):
		t.Fatal("next not handled")
	}
	cancel()
	<-done
}

func TestParseChatRef(t *testing.T) {
	t.Parallel()
	for _, test := range []struct {
		text string
		want ChatRef
		ok   bool
	}{
		{"whatsapp-main:120363@g.us", ChatRef{"whatsapp-main", "120363@g.us", ""}, true},
		{"telegram-main:-100123#42", ChatRef{"telegram-main", "-100123", "42"}, true},
		{"telegram-main:telegram:user:7", ChatRef{"telegram-main", "telegram:user:7", ""}, true},
		{"x:a%23b#c%25d", ChatRef{"x", "a#b", "c%d"}, true},
		{"x:a#", ChatRef{}, false},
		{":chat", ChatRef{}, false},
		{"nochat", ChatRef{}, false},
		{"x:", ChatRef{}, false},
		{"x:bad%2", ChatRef{}, false},
		{"x:bad%41", ChatRef{}, false},
	} {
		got, ok := ParseChatRef(test.text)
		if got != test.want || ok != test.ok {
			t.Errorf("ParseChatRef(%q) = %+v, %v; want %+v, %v", test.text, got, ok, test.want, test.ok)
		}
	}
	chat := ChatRef{"telegram-main", "-100123", "42"}
	if !(ChatRef{"telegram-main", "-100123", ""}).Covers(chat) || !(ChatRef{"telegram-main", "-100123", "42"}).Covers(chat) {
		t.Error("a ref must cover its chat, with or without the thread")
	}
	if (ChatRef{"telegram-main", "-100123", "9"}).Covers(chat) || (ChatRef{"telegram-alt", "-100123", ""}).Covers(chat) {
		t.Error("a ref covers another thread or connector")
	}
}
