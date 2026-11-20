// Package instagram is the Instagram downloader: a post or reel link posted in
// a configured chat is answered with the post's media, fetched by the
// Instagram connector through instagram.fetch_media@1.
package instagram

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/functions"
	"github.com/akshettrj/wuphf/misc/internal/config"
	"github.com/akshettrj/wuphf/misc/internal/misc"
	"github.com/rs/zerolog"
)

// FailureText is the reply to a failed fetch when reply_on_failure is set.
const FailureText = "Could not download this post."

// Invoker calls a function through the core's broker (*functions.Client
// satisfies it).
type Invoker interface {
	Invoke(ctx context.Context, name string, major uint32, args any, mediaIDs []string, result any) ([]string, error)
}

// Feature answers post and reel links in its chats.
type Feature struct {
	chats          []misc.ChatRef
	caption        bool
	replyOnFailure bool
	dedup          time.Duration
	perMinute      int
	timeout        time.Duration
	slots          chan struct{}
	invoker        Invoker
	logger         zerolog.Logger
	now            func() time.Time

	mu sync.Mutex
	// answered holds when each (chat, post) was last answered.
	answered map[string]time.Time
	// inFlight holds the (chat, post) pairs being fetched, so the same link
	// posted twice at once is fetched once.
	inFlight map[string]bool
	// fetches holds the start of each fetch in the last minute.
	fetches []time.Time
}

// New builds the feature from its config section. Every chat must be a valid
// chat reference.
func New(cfg config.Instagram, invoker Invoker, logger zerolog.Logger) (*Feature, error) {
	cfg = cfg.WithDefaults()
	if len(cfg.Chats) == 0 {
		return nil, errors.New("instagram: no chats configured")
	}
	chats := make([]misc.ChatRef, 0, len(cfg.Chats))
	for _, text := range cfg.Chats {
		ref, ok := misc.ParseChatRef(text)
		if !ok {
			return nil, fmt.Errorf("instagram: chats: %q is not a chat reference (connector:chat or connector:chat#thread)", text)
		}
		chats = append(chats, ref)
	}
	return &Feature{
		chats:          chats,
		caption:        *cfg.Caption,
		replyOnFailure: cfg.ReplyOnFailure,
		dedup:          cfg.Dedup(),
		perMinute:      int(cfg.MaxPerMinute),
		timeout:        cfg.CallTimeout(),
		slots:          make(chan struct{}, cfg.MaxConcurrent),
		invoker:        invoker,
		logger:         logger.With().Str("feature", "instagram").Logger(),
		now:            time.Now,
		answered:       map[string]time.Time{},
		inFlight:       map[string]bool{},
	}, nil
}

func (f *Feature) Name() string { return "instagram" }

// Handle answers each post or reel link in message, in order.
func (f *Feature) Handle(ctx context.Context, message misc.Message, out misc.Sender) {
	if !f.covers(message.Chat()) {
		return
	}
	for _, link := range functions.InstagramPostLinks(message.Text) {
		if ctx.Err() != nil {
			return
		}
		f.answer(ctx, message, link, out)
	}
}

func (f *Feature) covers(chat misc.ChatRef) bool {
	for _, ref := range f.chats {
		if ref.Covers(chat) {
			return true
		}
	}
	return false
}

func (f *Feature) answer(ctx context.Context, message misc.Message, link string, out misc.Sender) {
	shortcode, _ := functions.ParseInstagramPostLink(link)
	chat := message.Chat()
	key := chat.ConnectorID + "\x00" + chat.ChatID + "\x00" + chat.ThreadID + "\x00" + shortcode
	logger := f.logger.With().Str("chat", chat.ConnectorID+":"+chat.ChatID).Str("post", shortcode).Logger()
	if reason := f.claim(key); reason != "" {
		logger.Info().Msg("Instagram link skipped: " + reason)
		return
	}
	defer f.release(key)

	mediaIDs, result, err := f.fetch(ctx, link)
	if err != nil {
		if ctx.Err() != nil {
			return
		}
		logger.Warn().Err(err).Msg("Instagram post not downloaded")
		if f.replyOnFailure {
			if err := out.Reply(ctx, message, FailureText, nil); err != nil {
				logger.Warn().Err(err).Msg("failure reply not sent")
			}
		}
		return
	}
	if result.Skipped > 0 {
		logger.Warn().Int("skipped", result.Skipped).Msg("some of the post's media were too large or failed")
	}
	text := ""
	if f.caption {
		text = Caption(result.Author, result.Caption)
	}
	if err := out.Reply(ctx, message, text, mediaIDs); err != nil {
		logger.Warn().Err(err).Msg("Instagram post not sent")
		return
	}
	f.mu.Lock()
	f.answered[key] = f.now()
	f.mu.Unlock()
	logger.Info().Int("media", len(mediaIDs)).Msg("Instagram post sent")
}

// claim reserves key for one fetch. It returns why the link is skipped
// instead: answered within the dedup window, already being fetched, or over
// the per-minute limit.
func (f *Feature) claim(key string) string {
	f.mu.Lock()
	defer f.mu.Unlock()
	now := f.now()
	for answeredKey, at := range f.answered {
		if now.Sub(at) >= f.dedup {
			delete(f.answered, answeredKey)
		}
	}
	if _, ok := f.answered[key]; ok {
		return "already answered in this chat"
	}
	if f.inFlight[key] {
		return "already being fetched"
	}
	recent := f.fetches[:0]
	for _, at := range f.fetches {
		if now.Sub(at) < time.Minute {
			recent = append(recent, at)
		}
	}
	f.fetches = recent
	if len(f.fetches) >= f.perMinute {
		return fmt.Sprintf("over max_per_minute (%d)", f.perMinute)
	}
	f.fetches = append(f.fetches, now)
	f.inFlight[key] = true
	return ""
}

func (f *Feature) release(key string) {
	f.mu.Lock()
	delete(f.inFlight, key)
	f.mu.Unlock()
}

// fetch calls instagram.fetch_media@1 under the concurrency limit and the
// call timeout.
func (f *Feature) fetch(ctx context.Context, link string) ([]string, functions.InstagramFetchMediaResult, error) {
	var result functions.InstagramFetchMediaResult
	select {
	case f.slots <- struct{}{}:
	case <-ctx.Done():
		return nil, result, ctx.Err()
	}
	defer func() { <-f.slots }()
	callCtx, cancel := context.WithTimeout(ctx, f.timeout)
	defer cancel()
	mediaIDs, err := f.invoker.Invoke(callCtx, functions.InstagramFetchMediaName, functions.InstagramFetchMediaMajor,
		functions.InstagramFetchMediaArgs{URL: link}, nil, &result)
	if err != nil {
		return nil, result, err
	}
	if len(mediaIDs) == 0 {
		return nil, result, errors.New("the post has no media")
	}
	return mediaIDs, result, nil
}

// Caption is a reply's text: "@author: " and the post's text cut to
// config.CaptionMaxChars characters ("…" when cut).
func Caption(author, text string) string {
	text = strings.TrimSpace(text)
	if runes := []rune(text); len(runes) > config.CaptionMaxChars {
		text = strings.TrimRight(string(runes[:config.CaptionMaxChars]), " \n") + "…"
	}
	switch {
	case author == "":
		return text
	case text == "":
		return "@" + author
	default:
		return "@" + author + ": " + text
	}
}
