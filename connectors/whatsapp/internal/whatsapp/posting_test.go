package whatsappconnector

import (
	"context"
	"testing"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

type fakePostingDirectory struct {
	fakeDirectory
	groups   []*types.GroupInfo
	channels []*types.NewsletterMetadata
}

func (f fakePostingDirectory) GetJoinedGroups(context.Context) ([]*types.GroupInfo, error) {
	return f.groups, nil
}

func (f fakePostingDirectory) GetSubscribedNewsletters(context.Context) ([]*types.NewsletterMetadata, error) {
	return f.channels, nil
}

func (f fakePostingDirectory) GetStatusPrivacy(context.Context) ([]types.StatusPrivacy, error) {
	return []types.StatusPrivacy{{Type: types.StatusPrivacyTypeBlacklist, List: []types.JID{{}, {}, {}}}}, nil
}

func reporter(directory postingDirectory) (postingReporter, chan *wuphfpb.ClientFrame) {
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	return postingReporter{
		normalizer: NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil),
		directory:  directory,
		logger:     zerolog.Nop(),
	}, inputs
}

func postings(t *testing.T, inputs chan *wuphfpb.ClientFrame) map[string]*wuphfpb.ChatPosting {
	t.Helper()
	frame := <-inputs
	event := frame.GetChatUpdate()
	if !event.GetSnapshot() {
		t.Fatalf("posting facts are a snapshot: %v", event)
	}
	got := map[string]*wuphfpb.ChatPosting{}
	for _, update := range event.GetUpdates() {
		got[update.GetChatId()] = update.GetPosting()
	}
	return got
}

// The snapshot names the story feed receive-only with private replies, an
// admin-only group receive-only for a member, and a channel open only to
// its owner (plan 083).
func TestThePostingSnapshot(t *testing.T) {
	me := types.GroupParticipant{JID: types.NewJID("55", types.HiddenUserServer)}
	adminOnly := groupWith(true, &me)
	adminOnly.JID = types.NewJID("1", types.GroupServer)
	open := groupWith(false, &me)
	open.JID = types.NewJID("2", types.GroupServer)
	followed := &types.NewsletterMetadata{ID: types.NewJID("3", types.NewsletterServer), ViewerMeta: &types.NewsletterViewerMetadata{Role: types.NewsletterRoleSubscriber}}
	owned := &types.NewsletterMetadata{ID: types.NewJID("4", types.NewsletterServer), ViewerMeta: &types.NewsletterViewerMetadata{Role: types.NewsletterRoleOwner}}
	r, inputs := reporter(fakePostingDirectory{groups: []*types.GroupInfo{adminOnly, open}, channels: []*types.NewsletterMetadata{followed, owned}})
	if err := r.Snapshot(context.Background()); err != nil {
		t.Fatal(err)
	}
	got := postings(t, inputs)
	want := map[string]*wuphfpb.ChatPosting{
		"status@broadcast": {Messages: false, Reactions: true, PrivateReplies: true, Posts: true},
		"1@g.us":           {Messages: false, Reactions: true},
		"2@g.us":           {Messages: true, Reactions: true},
		"3@newsletter":     {Messages: false, Reactions: true},
		"4@newsletter":     {Messages: true, Reactions: true},
	}
	for chat, posting := range want {
		g := got[chat]
		if g == nil || g.Messages != posting.Messages || g.Reactions != posting.Reactions || g.PrivateReplies != posting.PrivateReplies || g.Posts != posting.Posts {
			t.Errorf("%s: got %v, want %v", chat, g, posting)
		}
	}
}

// A notification that turns a group admin-only, or promotes this account,
// reports the group again; another change reports nothing.
func TestAGroupChangeReportsPostingWhenItMatters(t *testing.T) {
	me := types.GroupParticipant{JID: types.NewJID("55", types.HiddenUserServer), IsAdmin: true}
	directory := fakePostingDirectory{fakeDirectory: fakeDirectory{info: groupWith(true, &me)}}
	r, inputs := reporter(directory)
	group := types.NewJID("1", types.GroupServer)

	if err := r.GroupChanged(context.Background(), &events.GroupInfo{JID: group, Name: &types.GroupName{Name: "x"}}); err != nil {
		t.Fatal(err)
	}
	if len(inputs) != 0 {
		t.Fatalf("a rename reported posting: %v", <-inputs)
	}
	promoted := &events.GroupInfo{JID: group, Promote: []types.JID{types.NewJID("55", types.HiddenUserServer)}}
	if err := r.GroupChanged(context.Background(), promoted); err != nil {
		t.Fatal(err)
	}
	if posting := postings(t, inputs)["1@g.us"]; posting == nil || !posting.Messages {
		t.Fatalf("an admin may post: %v", posting)
	}
	announce := &events.GroupInfo{JID: group, Announce: &types.GroupAnnounce{IsAnnounce: true}}
	if err := r.GroupChanged(context.Background(), announce); err != nil {
		t.Fatal(err)
	}
	if posting := postings(t, inputs)["1@g.us"]; posting == nil {
		t.Fatal("an announce change reported nothing")
	}
}

// The probe knows the story feed takes no messages.
func TestTheProbeKnowsStatusTakesNoMessages(t *testing.T) {
	facts := probeChat(context.Background(), fakeDirectory{}, "status@broadcast")
	if facts.GetCanSend() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("status@broadcast: %v", facts)
	}
}

// The story feed's audience is read from the status privacy (plan 084).
func TestTheSnapshotSaysWhoSeesAStory(t *testing.T) {
	r, inputs := reporter(fakePostingDirectory{})
	if err := r.Snapshot(context.Background()); err != nil {
		t.Fatal(err)
	}
	if audience := postings(t, inputs)["status@broadcast"].GetAudience(); audience != "My contacts except 3" {
		t.Fatalf("audience = %q", audience)
	}
}

func TestStatusAudience(t *testing.T) {
	cases := map[string][]types.StatusPrivacy{
		"":                     nil,
		"My contacts":          {{Type: types.StatusPrivacyTypeContacts}},
		"My contacts except 2": {{Type: types.StatusPrivacyTypeBlacklist, List: make([]types.JID, 2)}},
		"Only 1 contact":       {{Type: types.StatusPrivacyTypeWhitelist, List: make([]types.JID, 1)}},
		"Only 4 contacts":      {{Type: types.StatusPrivacyTypeWhitelist, List: make([]types.JID, 4)}},
	}
	for want, privacy := range cases {
		if got := statusAudience(privacy); got != want {
			t.Errorf("statusAudience(%v) = %q, want %q", privacy, got, want)
		}
	}
}
