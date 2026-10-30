package telegramconnector

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/gotd/td/telegram/uploader"
	"github.com/gotd/td/tg"
	"github.com/rs/zerolog"
)

// A user session cannot attach buttons to its own messages, but a message it
// sends from a bot's inline results carries that bot's buttons. So a labelled
// sticker goes through the helper bot (the one the settings panel uses): the
// helper uploads the sticker and answers an inline query with it and the label
// button, and the user session sends that result. One message, labelled like a
// bot session's sticker.
const (
	// stickerNoncePrefix marks the helper's inline queries that carry a
	// sticker, keeping them apart from the settings panel's tokens.
	stickerNoncePrefix = "sticker:"
	// inlineStickerBudget bounds the whole inline route (upload, query, send).
	// This connector delivers in order, so a stuck helper must not hold up
	// the chat: past the budget the sticker falls back to a label message.
	inlineStickerBudget = 5 * time.Second
	// stickerNonceLifetime bounds how long an unanswered nonce stays valid.
	stickerNonceLifetime = time.Minute
	// inlineStickerResultID names the one result the helper answers with.
	inlineStickerResultID = "sticker"
)

// errNoStickerHelper means there is no helper bot to send through.
var errNoStickerHelper = errors.New("no helper bot for inline stickers")

// InlineStickers sends labelled stickers from a user session through the
// helper bot. It has no route until the helper bot has logged in and the user
// session can address it (attach); until then Ready is false and stickers keep
// the label-message fallback.
type InlineStickers struct {
	logger zerolog.Logger
	now    func() time.Time
	// budget bounds one inline send (inlineStickerBudget; shorter in tests).
	budget time.Duration

	mu      sync.Mutex
	route   *inlineStickerRoute
	pending map[string]pendingSticker
}

// inlineStickerRoute is everything the inline route needs once the helper is
// up.
type inlineStickerRoute struct {
	// user is the user session's API: it asks for and sends the result.
	user *tg.Client
	// helper is the helper bot's API: it uploads and answers.
	helper *tg.Client
	// bot is the helper bot as the user session addresses it.
	bot tg.InputUserClass
	// owner is the user session's account id; the helper answers no one else.
	owner int64
}

// pendingSticker is a sticker waiting for its inline query.
type pendingSticker struct {
	data     []byte
	mime     string
	filename string
	label    string
	expires  time.Time
}

// NewInlineStickers returns an InlineStickers with no route yet.
func NewInlineStickers(logger zerolog.Logger) *InlineStickers {
	return &InlineStickers{logger: logger, now: time.Now, budget: inlineStickerBudget, pending: map[string]pendingSticker{}}
}

// attach gives the stickers their route once the helper bot is up.
func (s *InlineStickers) attach(user, helper *tg.Client, bot tg.InputUserClass, owner int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.route = &inlineStickerRoute{user: user, helper: helper, bot: bot, owner: owner}
}

// Ready says whether a sticker can go through the helper bot now.
func (s *InlineStickers) Ready() bool {
	if s == nil {
		return false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.route != nil
}

// Send sends data as a sticker labelled label to peer (replying to replyTo
// when it is not 0) as the helper bot's inline result, and returns the user
// session's updates: the sent sticker is the only message. Any error means
// nothing was sent, so the caller can fall back.
func (s *InlineStickers) Send(
	ctx context.Context,
	peer tg.InputPeerClass,
	replyTo int,
	data []byte,
	mime, filename, label string,
) (tg.UpdatesClass, error) {
	updates, err := s.send(ctx, peer, replyTo, data, mime, filename, label)
	if err != nil {
		s.logger.Warn().Err(err).Msg("inline sticker failed; sending a label message instead")
	}
	return updates, err
}

func (s *InlineStickers) send(
	ctx context.Context,
	peer tg.InputPeerClass,
	replyTo int,
	data []byte,
	mime, filename, label string,
) (tg.UpdatesClass, error) {
	nonce, err := newStickerNonce()
	if err != nil {
		return nil, err
	}
	s.mu.Lock()
	route := s.route
	if route != nil {
		s.pending[nonce] = pendingSticker{
			data:     data,
			mime:     mime,
			filename: filename,
			label:    label,
			expires:  s.now().Add(stickerNonceLifetime),
		}
	}
	s.mu.Unlock()
	if route == nil {
		return nil, errNoStickerHelper
	}
	// An unanswered nonce (the helper failed or was too slow) is dropped
	// here; an answered one is already gone.
	defer s.forget(nonce)

	results, err := route.user.MessagesGetInlineBotResults(ctx, &tg.MessagesGetInlineBotResultsRequest{
		Bot:   route.bot,
		Peer:  peer,
		Query: nonce,
	})
	if err != nil {
		return nil, fmt.Errorf("ask the helper bot for the sticker: %w", err)
	}
	if !hasResult(results, inlineStickerResultID) {
		return nil, errors.New("the helper bot answered without the sticker")
	}
	randomID, err := randomInt64()
	if err != nil {
		return nil, err
	}
	request := &tg.MessagesSendInlineBotResultRequest{
		Peer:     peer,
		RandomID: randomID,
		QueryID:  results.QueryID,
		ID:       inlineStickerResultID,
	}
	if replyTo != 0 {
		request.SetReplyTo(&tg.InputReplyToMessage{ReplyToMsgID: replyTo})
	}
	updates, err := route.user.MessagesSendInlineBotResult(ctx, request)
	if err != nil {
		return nil, fmt.Errorf("send the helper bot's sticker: %w", err)
	}
	return updates, nil
}

// forget drops a nonce that was not answered.
func (s *InlineStickers) forget(nonce string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.pending, nonce)
}

// take returns the sticker waiting under nonce for user and uses the nonce up.
// Another user's query gets nothing and leaves the nonce for the account.
func (s *InlineStickers) take(nonce string, user int64) (pendingSticker, *inlineStickerRoute, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	route := s.route
	pending, ok := s.pending[nonce]
	if !ok || route == nil || user != route.owner {
		return pendingSticker{}, nil, false
	}
	delete(s.pending, nonce)
	if s.now().After(pending.expires) {
		return pendingSticker{}, nil, false
	}
	return pending, route, true
}

// HandleInlineQuery answers the helper bot's inline queries that carry a
// sticker nonce, and reports whether the query was one. A known nonce from the
// account gets the sticker with its label button; anything else under the
// prefix gets an empty answer.
func (s *InlineStickers) HandleInlineQuery(ctx context.Context, helper *tg.Client, entities tg.Entities, update *tg.UpdateBotInlineQuery) (bool, error) {
	if s == nil || !strings.HasPrefix(update.Query, stickerNoncePrefix) {
		return false, nil
	}
	request := &tg.MessagesSetInlineBotResultsRequest{QueryID: update.QueryID, CacheTime: 0, Private: true}
	if pending, route, ok := s.take(update.Query, update.UserID); ok {
		document, err := uploadSticker(ctx, route.helper, ownerPeer(entities, update.UserID), pending)
		if err != nil {
			// Answer empty so the account's query fails at once instead of
			// waiting for Telegram's timeout.
			_, _ = helper.MessagesSetInlineBotResults(ctx, request)
			return true, err
		}
		request.Results = []tg.InputBotInlineResultClass{&tg.InputBotInlineResultDocument{
			ID:       inlineStickerResultID,
			Type:     "sticker",
			Document: document,
			SendMessage: &tg.InputBotInlineMessageMediaAuto{
				ReplyMarkup: labelMarkup(pending.label),
			},
		}}
	}
	_, err := helper.MessagesSetInlineBotResults(ctx, request)
	return true, err
}

// uploadSticker uploads a pending sticker through the helper bot and returns
// it as a document the helper can use in an inline result.
//
// messages.uploadMedia needs a peer, and which one a bot may use for a
// document it only puts in an inline result is not documented: InputPeerSelf
// is tried first, then the account itself, which the bot can address because
// the inline query just introduced it.
func uploadSticker(ctx context.Context, helper *tg.Client, account tg.InputPeerClass, pending pendingSticker) (*tg.InputDocument, error) {
	file, err := uploader.NewUploader(helper).FromBytes(ctx, pending.filename, pending.data)
	if err != nil {
		return nil, fmt.Errorf("upload the sticker to the helper bot: %w", err)
	}
	media := &tg.InputMediaUploadedDocument{
		File:     file,
		MimeType: pending.mime,
		Attributes: []tg.DocumentAttributeClass{
			&tg.DocumentAttributeFilename{FileName: pending.filename},
			&tg.DocumentAttributeSticker{Stickerset: &tg.InputStickerSetEmpty{}},
		},
	}
	var lastErr error
	for _, peer := range []tg.InputPeerClass{&tg.InputPeerSelf{}, account} {
		if peer == nil {
			continue
		}
		uploaded, err := helper.MessagesUploadMedia(ctx, &tg.MessagesUploadMediaRequest{Peer: peer, Media: media})
		if err != nil {
			lastErr = err
			continue
		}
		mediaDocument, ok := uploaded.(*tg.MessageMediaDocument)
		if !ok {
			return nil, fmt.Errorf("the helper bot's upload is a %T, not a document", uploaded)
		}
		document, ok := mediaDocument.Document.(*tg.Document)
		if !ok {
			return nil, errors.New("the helper bot's upload has no document")
		}
		return document.AsInput(), nil
	}
	return nil, fmt.Errorf("the helper bot cannot store the sticker: %w", lastErr)
}

// ownerPeer is the account as the helper bot sees it in an inline query.
func ownerPeer(entities tg.Entities, user int64) tg.InputPeerClass {
	if found, ok := entities.Users[user]; ok {
		return &tg.InputPeerUser{UserID: found.ID, AccessHash: found.AccessHash}
	}
	return nil
}

// labelMarkup is the one-button keyboard naming a sticker's sender; pressing
// it does nothing.
func labelMarkup(label string) *tg.ReplyInlineMarkup {
	return &tg.ReplyInlineMarkup{Rows: []tg.KeyboardButtonRow{{Buttons: []tg.KeyboardButtonClass{
		&tg.KeyboardButtonCallback{Text: label, Data: []byte(labelCallbackData)},
	}}}}
}

// hasResult says whether an inline answer offers the result id.
func hasResult(results *tg.MessagesBotResults, id string) bool {
	for _, result := range results.Results {
		if result.GetID() == id {
			return true
		}
	}
	return false
}

// newStickerNonce is a fresh, unguessable sticker query.
func newStickerNonce() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("make a sticker nonce: %w", err)
	}
	return stickerNoncePrefix + hex.EncodeToString(raw[:]), nil
}

// randomInt64 is a random message id for sendInlineBotResult.
func randomInt64() (int64, error) {
	var raw [8]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return 0, fmt.Errorf("make a random id: %w", err)
	}
	var value int64
	for _, b := range raw {
		value = value<<8 | int64(b)
	}
	return value, nil
}
