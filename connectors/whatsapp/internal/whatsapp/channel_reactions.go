package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"time"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// channelUpdatesPerRead bounds one reading of a channel's post updates.
const channelUpdatesPerRead = 100

// channelUpdates is the slice of the whatsmeow client that reads which of a
// channel's posts changed (their reaction and view counts).
type channelUpdates interface {
	GetNewsletterMessageUpdates(ctx context.Context, jid types.JID, params *whatsmeow.GetNewsletterUpdatesParams) ([]*types.NewsletterMessage, error)
}

// channelReactions reads the reaction counts of a channel's posts when the
// core asks (plan 085): the core polls each channel whose posting reports
// summaries, and composes the footer from what this returns.
type channelReactions struct {
	account func() channelUpdates
}

func newChannelReactions(client *linkedClient) channelReactions {
	return channelReactions{account: func() channelUpdates {
		if current := client.current.Load(); current != nil {
			return current
		}
		return nil
	}}
}

// ReadReactions reads the counts of the channel's posts that changed since
// `since`. Only a channel has them; any other chat reads none.
func (r channelReactions) ReadReactions(ctx context.Context, chatID string, since time.Time) ([]*wuphfpb.ReactionSummaryEvent, error) {
	jid, err := types.ParseJID(chatID)
	if err != nil {
		return nil, fmt.Errorf("reaction counts of %q: %w", chatID, err)
	}
	if jid.Server != types.NewsletterServer {
		return nil, nil
	}
	account := r.account()
	if account == nil {
		return nil, errors.New("no WhatsApp account is linked")
	}
	posts, err := account.GetNewsletterMessageUpdates(ctx, jid, &whatsmeow.GetNewsletterUpdatesParams{
		Count: channelUpdatesPerRead,
		Since: since,
	})
	if err != nil {
		return nil, fmt.Errorf("read the updates of channel %s: %w", chatID, err)
	}
	return reactionSummaries(chatID, posts), nil
}

// reactionSummaries reports one summary per post, named by the message id
// its copy has (a post without one is skipped), with its counts by emoji.
func reactionSummaries(chatID string, posts []*types.NewsletterMessage) []*wuphfpb.ReactionSummaryEvent {
	var out []*wuphfpb.ReactionSummaryEvent
	for _, post := range posts {
		if post == nil || post.MessageID == "" {
			continue
		}
		summary := &wuphfpb.ReactionSummaryEvent{ChatId: chatID, PlatformMessageId: post.MessageID}
		for emoji, count := range post.ReactionCounts {
			if count > 0 {
				summary.Counts = append(summary.Counts, &wuphfpb.ReactionCount{Emoji: emoji, Count: uint32(count)})
			}
		}
		sort.Slice(summary.Counts, func(i, j int) bool { return summary.Counts[i].Emoji < summary.Counts[j].Emoji })
		out = append(out, summary)
	}
	return out
}
