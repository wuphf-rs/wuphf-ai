package instagramconnector

import (
	"context"
	"strconv"
	"sync"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

// Normalizer turns lightspeed table deltas into core client frames. It is the
// single inbound translation point for the Instagram connector, covering message
// creates (with media), edits, and deletes/unsends.
//
// Two facts about the lightspeed protocol shape its caches:
//   - Thread titles arrive on their own thread rows, not on messages, but the
//     core only learns a source title through MessageCreateEvent.chat_title. The
//     name cache lets every create event carry the latest known title so mirror
//     threads get named and retitled.
//   - Edit rows (LSEditMessage) carry no thread key, yet the core needs a chat
//     id to resolve the origin endpoint. The message->thread cache supplies it.
type Normalizer struct {
	inputs chan<- *wuphfpb.ClientFrame
	media  MediaCodec
	// ownUserID is the logged-in account's user id (from the ds_user_id
	// cookie), used to label the account owner's own messages as "Me" so a
	// direct chat's two sides stay distinguishable on the mirror.
	ownUserID int64
	logger    zerolog.Logger

	mu             sync.Mutex
	threadNames    map[int64]string
	contactNames   map[int64]string
	messageThreads map[string]int64
}

func NewNormalizer(inputs chan<- *wuphfpb.ClientFrame, media MediaCodec, ownUserID int64, logger zerolog.Logger) *Normalizer {
	return &Normalizer{
		inputs:         inputs,
		media:          media,
		ownUserID:      ownUserID,
		logger:         logger,
		threadNames:    map[int64]string{},
		contactNames:   map[int64]string{},
		messageThreads: map[string]int64{},
	}
}

// Handle normalizes one lightspeed table delta. Thread and contact rows refresh
// the title caches first so any message in the same delta carries an up-to-date
// chat_title. The core owns echo suppression, so bridge-authored messages are
// submitted unconditionally.
func (n *Normalizer) Handle(ctx context.Context, tbl *table.LSTable) error {
	if tbl == nil {
		return nil
	}
	n.cacheTitles(tbl)
	bundles := collectMedia(tbl)
	if err := n.handleCreates(ctx, tbl, bundles); err != nil {
		return err
	}
	if err := n.handleEdits(ctx, tbl); err != nil {
		return err
	}
	return n.handleDeletes(ctx, tbl)
}

// cacheTitles records group thread names and contact names so chat titles can be
// resolved for both group threads (named directly) and one-to-one threads (whose
// title is the peer contact's name, keyed by the thread key == contact id).
func (n *Normalizer) cacheTitles(tbl *table.LSTable) {
	n.mu.Lock()
	defer n.mu.Unlock()
	for _, thread := range tbl.LSDeleteThenInsertThread {
		if thread.ThreadName != "" {
			n.threadNames[thread.ThreadKey] = thread.ThreadName
		}
	}
	for _, thread := range tbl.LSSyncUpdateThreadName {
		if name := firstNonEmpty(thread.ThreadName, thread.ThreadName1); name != "" {
			n.threadNames[thread.ThreadKey] = name
		}
	}
	for _, contact := range tbl.LSVerifyContactRowExists {
		if contact.Name != "" {
			n.contactNames[contact.ContactId] = contact.Name
		}
	}
	for _, contact := range tbl.LSDeleteThenInsertContact {
		if contact.Name != "" {
			n.contactNames[contact.Id] = contact.Name
		}
	}
}

func (n *Normalizer) handleCreates(ctx context.Context, tbl *table.LSTable, bundles map[string]*mediaBundle) error {
	for _, message := range tbl.LSInsertMessage {
		if message.MessageId == "" {
			continue
		}
		n.recordMessageThread(message.MessageId, message.ThreadKey)
		if message.IsUnsent {
			if err := n.emitDelete(ctx, message.ThreadKey, message.MessageId); err != nil {
				return err
			}
			continue
		}
		if err := n.emitCreate(ctx, message, bundles[message.MessageId]); err != nil {
			return err
		}
	}
	return nil
}

func (n *Normalizer) emitCreate(ctx context.Context, message *table.LSInsertMessage, bundle *mediaBundle) error {
	text := message.Text
	var attachments []*wuphfpb.Attachment
	if bundle != nil {
		text = appendLine(text, bundle.suffix)
		if len(bundle.attachments) > 0 {
			fetched, err := n.media.fetchAll(ctx, bundle.attachments)
			if err != nil {
				// A bad CDN URL must not tear down the session and lose the
				// message on reconnect; bridge the text and drop the media.
				n.logger.Err(err).Str("message_id", message.MessageId).Msg("dropping Instagram attachments after download failure")
			} else {
				attachments = fetched
			}
		}
	}
	if text == "" && len(attachments) == 0 {
		return nil
	}
	// The participant is set for every message so mirror targets label the
	// author; without it a direct chat's two sides are indistinguishable.
	participant := strconv.FormatInt(message.SenderId, 10)
	event := &wuphfpb.MessageCreateEvent{
		ChatId:            strconv.FormatInt(message.ThreadKey, 10),
		ChatTitle:         n.chatTitle(message.ThreadKey),
		PlatformMessageId: message.MessageId,
		Sender:            strconv.FormatInt(message.SenderId, 10),
		Participant:       &participant,
		TimestampMs:       message.TimestampMs,
		Attachments:       attachments,
	}
	if name := n.senderLabel(message.SenderId); name != "" {
		event.SenderName = &name
	}
	if text != "" {
		event.Text = &text
	}
	if message.ReplySourceId != "" {
		reply := message.ReplySourceId
		event.ReplyToPlatformMessageId = &reply
	}
	return n.send(ctx, &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageCreate{MessageCreate: event},
	})
}

// handleEdits bridges in-place edits. LSEditMessage omits the thread key, so the
// chat id is recovered from the message->thread cache; an edit whose original
// insert was never observed (cache miss) cannot be resolved and is skipped.
func (n *Normalizer) handleEdits(ctx context.Context, tbl *table.LSTable) error {
	for _, edit := range tbl.LSEditMessage {
		threadKey, ok := n.messageThread(edit.MessageID)
		if !ok {
			n.logger.Warn().Str("message_id", edit.MessageID).Msg("skipping Instagram edit for unknown message")
			continue
		}
		text := edit.Text
		event := &wuphfpb.MessageEditEvent{
			ChatId:            strconv.FormatInt(threadKey, 10),
			PlatformMessageId: edit.MessageID,
			Text:              &text,
		}
		if err := n.send(ctx, &wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_MessageEdit{MessageEdit: event},
		}); err != nil {
			return err
		}
	}
	return nil
}

func (n *Normalizer) handleDeletes(ctx context.Context, tbl *table.LSTable) error {
	for _, del := range tbl.LSDeleteMessage {
		if del.MessageId == "" {
			continue
		}
		if err := n.emitDelete(ctx, del.ThreadKey, del.MessageId); err != nil {
			return err
		}
	}
	return nil
}

func (n *Normalizer) emitDelete(ctx context.Context, threadKey int64, messageID string) error {
	event := &wuphfpb.MessageDeleteEvent{
		ChatId:            strconv.FormatInt(threadKey, 10),
		PlatformMessageId: messageID,
	}
	return n.send(ctx, &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageDelete{MessageDelete: event},
	})
}

// senderLabel resolves a message author's display label: the account owner's
// own messages are labelled "Me", others resolve through the contact-name
// cache. Returns "" when the sender is unknown, in which case mirror targets
// fall back to the raw participant id.
func (n *Normalizer) senderLabel(senderID int64) string {
	if n.ownUserID != 0 && senderID == n.ownUserID {
		return "Me"
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	return n.contactNames[senderID]
}

// chatTitle resolves a thread's display name, preferring an explicit group name
// and falling back to the peer contact's name for one-to-one threads (whose key
// equals the contact id). Returns nil when no name is known.
func (n *Normalizer) chatTitle(threadKey int64) *string {
	n.mu.Lock()
	defer n.mu.Unlock()
	if name := n.threadNames[threadKey]; name != "" {
		return &name
	}
	if name := n.contactNames[threadKey]; name != "" {
		return &name
	}
	return nil
}

func (n *Normalizer) recordMessageThread(messageID string, threadKey int64) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.messageThreads[messageID] = threadKey
}

func (n *Normalizer) messageThread(messageID string) (int64, bool) {
	n.mu.Lock()
	defer n.mu.Unlock()
	threadKey, ok := n.messageThreads[messageID]
	return threadKey, ok
}

func (n *Normalizer) send(ctx context.Context, frame *wuphfpb.ClientFrame) error {
	select {
	case n.inputs <- frame:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
