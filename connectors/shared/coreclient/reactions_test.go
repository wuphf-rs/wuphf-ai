package coreclient

import (
	"context"
	"errors"
	"testing"
	"time"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

type fakeReader struct {
	chat  string
	since time.Time
	err   error
}

func (f *fakeReader) ReadReactions(_ context.Context, chatID string, since time.Time) ([]*wuphfpb.ReactionSummaryEvent, error) {
	f.chat, f.since = chatID, since
	return []*wuphfpb.ReactionSummaryEvent{{ChatId: chatID, PlatformMessageId: "p1"}}, f.err
}

func TestARefreshIsAnsweredWithOneSummaryPerPost(t *testing.T) {
	reader := &fakeReader{}
	frames, err := reactionFrames(context.Background(), reader, &wuphfpb.RefreshReactions{ChatId: "1@newsletter", SinceMs: 5_000})
	if err != nil || len(frames) != 1 || frames[0].GetReactionSummary().GetPlatformMessageId() != "p1" {
		t.Fatalf("frames = %v, err = %v", frames, err)
	}
	if reader.chat != "1@newsletter" || !reader.since.Equal(time.UnixMilli(5_000)) {
		t.Fatalf("asked %q since %v", reader.chat, reader.since)
	}
	// A reader that fails sends nothing; none reads nothing.
	reader.err = errors.New("offline")
	if frames, err := reactionFrames(context.Background(), reader, &wuphfpb.RefreshReactions{}); err == nil || frames != nil {
		t.Fatalf("a failed read: %v, %v", frames, err)
	}
	if frames, err := reactionFrames(context.Background(), nil, &wuphfpb.RefreshReactions{}); err != nil || frames != nil {
		t.Fatalf("no reader: %v, %v", frames, err)
	}
}
