package coreclient

import (
	"context"
	"time"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// ReactionReader reads the anonymous reaction counts of a chat's posts (plan
// 085: a WhatsApp channel). The core asks with a RefreshReactions for each
// chat whose posting reports summaries, every channels.counts_poll_minutes;
// the connector answers with one ReactionSummaryEvent per post it read. The
// core keeps the counts and decides what to edit; a connector only reads.
type ReactionReader interface {
	// ReadReactions returns the counts of chatID's posts whose counts changed
	// since `since`. An error sends nothing: the core asks again later.
	ReadReactions(ctx context.Context, chatID string, since time.Time) ([]*wuphfpb.ReactionSummaryEvent, error)
}

// reactionFrames asks reader (when set) for a RefreshReactions' counts and
// returns them as frames, none when it cannot read them.
func reactionFrames(ctx context.Context, reader ReactionReader, refresh *wuphfpb.RefreshReactions) ([]*wuphfpb.ClientFrame, error) {
	if reader == nil {
		return nil, nil
	}
	summaries, err := reader.ReadReactions(ctx, refresh.GetChatId(), time.UnixMilli(refresh.GetSinceMs()))
	if err != nil {
		return nil, err
	}
	frames := make([]*wuphfpb.ClientFrame, 0, len(summaries))
	for _, summary := range summaries {
		frames = append(frames, &wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_ReactionSummary{ReactionSummary: summary}})
	}
	return frames, nil
}
