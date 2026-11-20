package misc

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// Observer is the part of the core's observer service wuphf-misc uses
// (wuphfpb.ObserverTransportClient satisfies it).
type Observer interface {
	SubscribeMessages(ctx context.Context, in *wuphfpb.SubscribeMessagesRequest, opts ...grpc.CallOption) (grpc.ServerStreamingClient[wuphfpb.ObservedMessage], error)
	SendMessage(ctx context.Context, in *wuphfpb.SendMessageRequest, opts ...grpc.CallOption) (*wuphfpb.SendMessageResponse, error)
}

// seenFor is how long the runner remembers a message it handed to features.
// The core streams a message again when its row changes: a send's settled row
// and then the platform's echo of it, or an echo that arrived before the
// acknowledgement and then the settled row. Each reaches features once.
const seenFor = 10 * time.Minute

// ownTextFor is how long a reply's text marks a message in the same chat as
// the reply itself. The platform can report a send back before the core has
// its acknowledgement; that early echo is incoming, without sent_by.
const ownTextFor = 2 * time.Minute

// Runner streams the core's live messages and hands each one that may trigger
// features to every feature.
type Runner struct {
	observer       Observer
	features       []Feature
	logger         zerolog.Logger
	reconnectDelay time.Duration
	now            func() time.Time

	mu      sync.Mutex
	seen    map[string]time.Time
	ownText map[string]time.Time
}

func NewRunner(observer Observer, features []Feature, reconnectDelay time.Duration, logger zerolog.Logger) *Runner {
	return &Runner{
		observer:       observer,
		features:       features,
		logger:         logger,
		reconnectDelay: reconnectDelay,
		now:            time.Now,
		seen:           map[string]time.Time{},
		ownText:        map[string]time.Time{},
	}
}

// Run streams messages until ctx ends, resubscribing with a pause whenever the
// stream ends. Messages sent while no stream is open are not seen (features
// act on live messages only). Run waits for the features' work before it
// returns. A rejected token ends Run with an error.
func (r *Runner) Run(ctx context.Context) error {
	var work sync.WaitGroup
	defer work.Wait()
	for {
		err := r.stream(ctx, &work)
		if ctx.Err() != nil {
			return ctx.Err()
		}
		switch status.Code(err) {
		case codes.Unauthenticated, codes.PermissionDenied:
			return fmt.Errorf("core rejected the observer token: %w", err)
		}
		r.logger.Warn().Err(err).Dur("retry_in", r.reconnectDelay).Msg("message stream ended; resubscribing")
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(r.reconnectDelay):
		}
	}
}

func (r *Runner) stream(ctx context.Context, work *sync.WaitGroup) error {
	stream, err := r.observer.SubscribeMessages(ctx, &wuphfpb.SubscribeMessagesRequest{})
	if err != nil {
		return err
	}
	r.logger.Info().Msg("subscribed to the core's messages")
	for {
		observed, err := stream.Recv()
		if err != nil {
			return err
		}
		if observed.GetEdited() || observed.GetDeleted() {
			continue
		}
		message := messageFromProto(observed)
		if !r.admit(message) {
			continue
		}
		for _, feature := range r.features {
			work.Go(func() {
				feature.Handle(ctx, message, featureSender{runner: r, feature: feature.Name()})
			})
		}
	}
}

// admit reports whether message may trigger features: a message arrived from
// the platform (the owner's own, from the official apps, included) or sent
// out by another client (the TUI), seen for the first time. wuphf-misc's own
// replies never trigger, nor does their early echo.
func (r *Runner) admit(message Message) bool {
	switch message.Direction {
	case Incoming:
	case Outgoing:
		if message.SentBy == ClientName {
			r.remember(message.key())
			return false
		}
	default:
		return false
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	now := r.now()
	r.forgetBefore(now)
	if _, ok := r.seen[message.key()]; ok {
		return false
	}
	r.seen[message.key()] = now
	if message.Text != "" {
		if _, ok := r.ownText[ownTextKey(message.Chat(), message.Text)]; ok {
			return false
		}
	}
	return true
}

func (r *Runner) remember(key string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.seen[key] = r.now()
}

// forgetBefore drops what is older than its window. The caller holds mu.
func (r *Runner) forgetBefore(now time.Time) {
	for key, at := range r.seen {
		if now.Sub(at) > seenFor {
			delete(r.seen, key)
		}
	}
	for key, at := range r.ownText {
		if now.Sub(at) > ownTextFor {
			delete(r.ownText, key)
		}
	}
}

func ownTextKey(chat ChatRef, text string) string {
	return chat.ConnectorID + "\x00" + chat.ChatID + "\x00" + chat.ThreadID + "\x00" + text
}

// featureSender posts a feature's replies as wuphf-misc.
type featureSender struct {
	runner  *Runner
	feature string
}

func (s featureSender) Reply(ctx context.Context, to Message, text string, mediaIDs []string) error {
	if text == "" && len(mediaIDs) == 0 {
		return errors.New("a reply needs text or media")
	}
	r := s.runner
	if text != "" {
		r.mu.Lock()
		r.ownText[ownTextKey(to.Chat(), text)] = r.now()
		r.mu.Unlock()
	}
	request := &wuphfpb.SendMessageRequest{
		ConnectorId:              to.ConnectorID,
		ChatId:                   to.ChatID,
		Text:                     text,
		ReplyToPlatformMessageId: &to.NativeID,
		MediaIds:                 mediaIDs,
		Client:                   new(ClientName),
	}
	if to.ThreadID != "" {
		request.ThreadId = &to.ThreadID
	}
	response, err := r.observer.SendMessage(ctx, request)
	if err != nil {
		return fmt.Errorf("%s: reply in %s: %w", s.feature, to.ChatID, err)
	}
	if id := response.GetPlatformMessageId(); id != "" {
		r.remember(Message{ConnectorID: to.ConnectorID, ChatID: to.ChatID, NativeID: id}.key())
	}
	return nil
}
