package whatsappconnector

import (
	"context"
	"testing"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

type fakeChannelUpdates struct {
	asked  types.JID
	params *whatsmeow.GetNewsletterUpdatesParams
	posts  []*types.NewsletterMessage
}

func (f *fakeChannelUpdates) GetNewsletterMessageUpdates(_ context.Context, jid types.JID, params *whatsmeow.GetNewsletterUpdatesParams) ([]*types.NewsletterMessage, error) {
	f.asked, f.params = jid, params
	return f.posts, nil
}

func TestAChannelsChangedPostsAreReadAsSummaries(t *testing.T) {
	t.Parallel()
	updates := &fakeChannelUpdates{posts: []*types.NewsletterMessage{
		{MessageID: "p1", ReactionCounts: map[string]int{"👍": 34, "❤️": 120, "😂": 0}},
		{MessageID: "p2"},
		{MessageServerID: 7}, // no id: skipped
	}}
	reader := channelReactions{account: func() channelUpdates { return updates }}
	since := time.UnixMilli(1_700_000_000_000)
	summaries, err := reader.ReadReactions(context.Background(), "123@newsletter", since)
	if err != nil {
		t.Fatal(err)
	}
	if updates.asked.String() != "123@newsletter" || !updates.params.Since.Equal(since) || updates.params.Count != channelUpdatesPerRead {
		t.Fatalf("asked %v with %+v", updates.asked, updates.params)
	}
	if len(summaries) != 2 || summaries[0].GetPlatformMessageId() != "p1" || len(summaries[1].GetCounts()) != 0 {
		t.Fatalf("summaries = %v", summaries)
	}
	counts := summaries[0].GetCounts()
	if len(counts) != 2 || counts[0].GetEmoji() != "❤️" || counts[0].GetCount() != 120 || counts[1].GetCount() != 34 {
		t.Fatalf("counts = %v", counts)
	}
}

func TestOnlyAChannelHasReactionCounts(t *testing.T) {
	t.Parallel()
	reader := channelReactions{account: func() channelUpdates { return nil }}
	if summaries, err := reader.ReadReactions(context.Background(), "1@g.us", time.Now()); err != nil || summaries != nil {
		t.Fatalf("a group: %v, %v", summaries, err)
	}
	if _, err := reader.ReadReactions(context.Background(), "1@newsletter", time.Now()); err == nil {
		t.Fatal("no linked account reads nothing, with an error")
	}
}
