package instagramconnector

import (
	"cmp"
	"context"
	"errors"
	"strconv"
	"sync"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
)

// reactionTargetLimit bounds the reaction cache; past it the cache is reset,
// which only costs un-reacting to reactions older than the reset.
const reactionTargetLimit = 4096

// Normalizer turns Instagram deltas into core client frames. It is the single
// inbound translation point for the Instagram connector, covering message
// creates (with media), edits, deletes/unsends and reactions.
//
// Every delta names its thread by the short IG id; the thread directory maps it
// to the thread key the core knows the chat by, and supplies the chat title
// that messages themselves do not carry.
type Normalizer struct {
	inputs  chan<- *wuphfpb.ClientFrame
	media   MediaCodec
	threads *ThreadDirectory
	// ownUserID is the logged-in account's user id, used to label the account
	// owner's own messages as "Me" so a direct chat's two sides stay
	// distinguishable on the mirror.
	ownUserID int64
	logger    zerolog.Logger

	mu sync.Mutex
	// reactionTargets maps a reaction's log message id to what it reacted to:
	// a reaction removal sometimes names only that id.
	reactionTargets map[string]reactionTarget
}

type reactionTarget struct {
	messageID string
	senderID  int64
}

func NewNormalizer(inputs chan<- *wuphfpb.ClientFrame, media MediaCodec, threads *ThreadDirectory, ownUserID int64, logger zerolog.Logger) *Normalizer {
	return &Normalizer{
		inputs:          inputs,
		media:           media,
		threads:         threads,
		ownUserID:       ownUserID,
		logger:          logger,
		reactionTargets: map[string]reactionTarget{},
	}
}

// Handle normalizes one delta. Only a canceled context is returned as an
// error: the client then leaves the delta unacknowledged and redelivers it
// after reconnecting. Anything else that stops one delta from bridging is
// logged and skipped so it cannot wedge the stream. The core owns echo
// suppression, so bridge-authored messages are submitted unconditionally.
func (n *Normalizer) Handle(ctx context.Context, delta *slidetypes.Delta) error {
	if delta == nil {
		return nil
	}
	shortID := delta.ThreadIGID
	// Some messages leave the delta's thread id empty and only carry it on the
	// message itself.
	if event, ok := delta.Data.(*slidetypes.NewMessageEvent); ok && shortID == "" && event.Message != nil {
		shortID = event.Message.ThreadFBID
	}
	switch delta.Data.(type) {
	case *slidetypes.NewMessageEvent, *slidetypes.AdminMessageEvent, *slidetypes.EditMessageEvent,
		*slidetypes.DeleteMessageEvent, *slidetypes.CreateReactionEvent, *slidetypes.DeleteReactionEvent,
		*slidetypes.UpdateThreadNameEvent, *slidetypes.ParticipantJoinEvent, *slidetypes.ParticipantLeaveEvent:
	default:
		n.logger.Trace().Str("delta_type", delta.TypeName).Msg("ignoring Instagram delta")
		return nil
	}
	threadKey, err := n.threads.ResolveShort(ctx, shortID)
	if err != nil {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		n.logger.Warn().Err(err).Str("delta_type", delta.TypeName).Str("thread_id", shortID).Msg("skipping Instagram delta for unresolvable thread")
		return nil
	}
	err = n.dispatch(ctx, threadKey, shortID, delta.Data)
	if err != nil && !errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded) {
		n.logger.Warn().Err(err).Str("delta_type", delta.TypeName).Msg("skipping Instagram delta")
		return nil
	}
	return err
}

func (n *Normalizer) dispatch(ctx context.Context, threadKey int64, shortID string, data slidetypes.DeltaEvent) error {
	switch event := data.(type) {
	case *slidetypes.NewMessageEvent:
		return n.emitCreate(ctx, threadKey, event.Message)
	case *slidetypes.AdminMessageEvent:
		return n.emitCreate(ctx, threadKey, event.Message)
	case *slidetypes.EditMessageEvent:
		return n.emitEdit(ctx, threadKey, event)
	case *slidetypes.DeleteMessageEvent:
		return n.emitDelete(ctx, threadKey, event.MessageID)
	case *slidetypes.CreateReactionEvent:
		n.rememberReaction(event.Reaction.LogMessageID, event.MessageID, event.Reaction.SenderFBID)
		return n.emitReaction(ctx, threadKey, event.MessageID, event.Reaction.SenderFBID, event.Reaction.Reaction)
	case *slidetypes.DeleteReactionEvent:
		return n.emitReactionRemoval(ctx, threadKey, event)
	case *slidetypes.UpdateThreadNameEvent:
		// The next message carries the new title; the admin line announcing
		// the rename is bridged like any other.
		n.threads.RecordTitle(shortID, event.ThreadName)
		return n.emitCreate(ctx, threadKey, event.Message)
	case *slidetypes.ParticipantJoinEvent:
		if thread := event.Thread.AsIGDirectThread; thread != nil {
			for _, user := range thread.Users {
				n.threads.RecordUser(user)
			}
			n.threads.RecordTitle(shortID, thread.ThreadTitle)
		}
		return n.emitCreate(ctx, threadKey, event.Message)
	case *slidetypes.ParticipantLeaveEvent:
		if thread := event.Thread.AsIGDirectThread; thread != nil {
			n.threads.RecordTitle(shortID, thread.ThreadTitle)
		}
		return n.emitCreate(ctx, threadKey, event.Message)
	}
	return nil
}

func (n *Normalizer) emitCreate(ctx context.Context, threadKey int64, message *slidetypes.Message) error {
	if message == nil || message.ID == "" {
		return nil
	}
	if message.Sender != nil {
		n.threads.RecordUser(&message.Sender.UserDict)
	}
	body := bodyOf(message)
	var attachments []*wuphfpb.Attachment
	if len(body.media) > 0 {
		fetched, err := n.media.fetchAll(ctx, body.media)
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			// A bad CDN URL must not drop the whole message; bridge the text
			// and drop the media.
			n.logger.Err(err).Str("message_id", message.ID).Msg("dropping Instagram attachments after download failure")
		} else {
			attachments = fetched
		}
	}
	if body.text == "" && len(attachments) == 0 {
		return nil
	}
	// The participant is set for every message so mirror targets label the
	// author; without it a direct chat's two sides are indistinguishable.
	sender := strconv.FormatInt(message.SenderFBID, 10)
	event := &wuphfpb.MessageCreateEvent{
		ChatId:            formatThreadKey(threadKey),
		ChatTitle:         n.threads.ChatTitle(threadKey),
		PlatformMessageId: message.ID,
		Sender:            sender,
		Participant:       &sender,
		TimestampMs:       message.TimestampMS.UnixMilli(),
		Attachments:       attachments,
	}
	if name := n.senderLabel(message.SenderFBID, message.Sender); name != "" {
		event.SenderName = &name
	}
	if body.text != "" {
		event.Text = &body.text
	}
	if message.RepliedToMessageID != "" {
		reply := message.RepliedToMessageID
		event.ReplyToPlatformMessageId = &reply
	}
	return n.send(ctx, &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageCreate{MessageCreate: event},
	})
}

func (n *Normalizer) emitEdit(ctx context.Context, threadKey int64, edit *slidetypes.EditMessageEvent) error {
	if edit.MessageID == "" {
		return nil
	}
	text := edit.TextBody
	return n.send(ctx, &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageEdit{MessageEdit: &wuphfpb.MessageEditEvent{
			ChatId:            formatThreadKey(threadKey),
			PlatformMessageId: edit.MessageID,
			Text:              &text,
		}},
	})
}

func (n *Normalizer) emitDelete(ctx context.Context, threadKey int64, messageID string) error {
	if messageID == "" {
		return nil
	}
	return n.send(ctx, &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_MessageDelete{MessageDelete: &wuphfpb.MessageDeleteEvent{
			ChatId:            formatThreadKey(threadKey),
			PlatformMessageId: messageID,
		}},
	})
}

// emitReactionRemoval clears a reactor's emoji. Instagram keys reactions by
// (message, actor), matching the core's per-reactor toggle model; when the
// removal names only the reaction's own log id, the target is recovered from
// the reaction seen earlier.
func (n *Normalizer) emitReactionRemoval(ctx context.Context, threadKey int64, removal *slidetypes.DeleteReactionEvent) error {
	messageID, senderID := removal.MessageID, removal.Reaction.SenderFBID
	if senderID == 0 {
		target, ok := n.reactionTarget(removal.Reaction.LogMessageID)
		if !ok {
			n.logger.Warn().Str("reaction_id", removal.Reaction.LogMessageID).Msg("skipping removal of unknown Instagram reaction")
			return nil
		}
		messageID, senderID = target.messageID, target.senderID
	}
	return n.emitReaction(ctx, threadKey, messageID, senderID, "")
}

func (n *Normalizer) emitReaction(ctx context.Context, threadKey int64, messageID string, actorID int64, emoji string) error {
	if messageID == "" {
		return nil
	}
	event := &wuphfpb.ReactionEvent{
		ChatId:            formatThreadKey(threadKey),
		PlatformMessageId: messageID,
		Sender:            strconv.FormatInt(actorID, 10),
		Emoji:             emoji,
		FromMe:            n.ownUserID != 0 && actorID == n.ownUserID,
	}
	if name := n.senderLabel(actorID, nil); name != "" {
		event.SenderName = &name
	}
	return n.send(ctx, &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_Reaction{Reaction: event},
	})
}

// senderLabel resolves an author's display label: the account owner's own
// messages are labelled "Me", others use the name the message carries or the
// directory's. Returns "" when unknown, in which case mirror targets fall back
// to the raw participant id.
func (n *Normalizer) senderLabel(senderID int64, sender *slidetypes.MessageSender) string {
	if n.ownUserID != 0 && senderID == n.ownUserID {
		return "Me"
	}
	if sender != nil {
		if name := cmp.Or(sender.UserDict.FullName, sender.Name, sender.UserDict.Username); name != "" {
			return name
		}
	}
	return n.threads.UserName(senderID)
}

func (n *Normalizer) rememberReaction(logID, messageID string, senderID int64) {
	if logID == "" {
		return
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if len(n.reactionTargets) >= reactionTargetLimit {
		clear(n.reactionTargets)
	}
	n.reactionTargets[logID] = reactionTarget{messageID: messageID, senderID: senderID}
}

func (n *Normalizer) reactionTarget(logID string) (reactionTarget, bool) {
	n.mu.Lock()
	defer n.mu.Unlock()
	target, ok := n.reactionTargets[logID]
	return target, ok && logID != ""
}

func (n *Normalizer) send(ctx context.Context, frame *wuphfpb.ClientFrame) error {
	select {
	case n.inputs <- frame:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
