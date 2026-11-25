package whatsappconnector

import (
	"context"
	"testing"

	"github.com/wuphf-rs/wuphf-ai/connectors/shared/chatdir"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// A community of two groups: "Neighbours" (9@g.us) holds "Garden" (1@g.us).
func community() (parent, member, loner *types.GroupInfo) {
	parent = &types.GroupInfo{JID: types.NewJID("9", types.GroupServer), GroupName: types.GroupName{Name: "Neighbours"},
		GroupParent: types.GroupParent{IsParent: true}}
	member = &types.GroupInfo{JID: types.NewJID("1", types.GroupServer), GroupName: types.GroupName{Name: "Garden"},
		GroupLinkedParent: types.GroupLinkedParent{LinkedParentJID: parent.JID}}
	loner = &types.GroupInfo{JID: types.NewJID("2", types.GroupServer), GroupName: types.GroupName{Name: "Book club"}}
	return parent, member, loner
}

func TestAGroupInACommunityIsTitledAndReportedUnderIt(t *testing.T) {
	t.Parallel()
	parent, member, loner := community()
	names := groupNames([]*types.GroupInfo{parent, member, loner})
	for _, tc := range []struct {
		group     *types.GroupInfo
		title     string
		community string
	}{
		{member, "[G] Neighbours › Garden", "9@g.us"},
		{loner, "[G] Book club", ""},
		{parent, "[G] Neighbours", ""},
	} {
		if got := groupTitle(tc.group, names); got != tc.title {
			t.Errorf("%s: title %q, want %q", tc.group.Name, got, tc.title)
		}
		got := groupCommunity(tc.group, names)
		if got.GetChatId() != tc.community || (tc.community != "" && got.GetTitle() != "Neighbours") {
			t.Errorf("%s: community %v, want %q", tc.group.Name, got, tc.community)
		}
	}
	// A community whose name is unknown leaves the group's own.
	if got := groupTitle(member, func(types.JID) string { return "" }); got != "[G] Garden" {
		t.Errorf("unknown community: %q", got)
	}
}

func TestALinkOrUnlinkReportsTheCommunityAndTheNewTitle(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	namer := &recordingNamer{titles: map[string]string{"1@g.us": "[G] Neighbours › Garden"}}
	normalizer := NewNormalizer(inputs, MediaCodec{}, namer, nil, nil, nil)
	group := types.NewJID("1", types.GroupServer)
	parent := types.GroupLinkTarget{JID: types.NewJID("9", types.GroupServer), GroupName: types.GroupName{Name: "Neighbours"}}

	link := &events.GroupInfo{JID: group, Link: &types.GroupLinkChange{Type: types.GroupLinkChangeTypeParent, Group: parent}}
	if err := normalizer.HandleGroupInfo(context.Background(), link); err != nil {
		t.Fatal(err)
	}
	update := nextChatUpdate(t, inputs).GetUpdates()[0]
	if update.GetCommunity().GetChatId() != "9@g.us" || update.GetCommunity().GetTitle() != "Neighbours" ||
		update.GetTitle() != "[G] Neighbours › Garden" {
		t.Fatalf("link: %v", update)
	}
	if len(namer.forgotten) != 1 {
		t.Fatalf("the group's title is read again: %v", namer.forgotten)
	}

	namer.titles["1@g.us"] = "[G] Garden"
	unlink := &events.GroupInfo{JID: group, Unlink: &types.GroupLinkChange{Type: types.GroupLinkChangeTypeParent, Group: parent}}
	if err := normalizer.HandleGroupInfo(context.Background(), unlink); err != nil {
		t.Fatal(err)
	}
	update = nextChatUpdate(t, inputs).GetUpdates()[0]
	if update.Community == nil || update.GetCommunity().GetChatId() != "" || update.GetTitle() != "[G] Garden" {
		t.Fatalf("unlink: %v", update)
	}

	// A sub-group linked to a community (seen from the community) is not
	// this group's community.
	sub := &events.GroupInfo{JID: group, Link: &types.GroupLinkChange{Type: types.GroupLinkChangeTypeSub, Group: parent}}
	if err := normalizer.HandleGroupInfo(context.Background(), sub); err != nil {
		t.Fatal(err)
	}
	if len(inputs) != 0 {
		t.Fatalf("a sub-group link reported: %v", <-inputs)
	}
}

func TestTheSnapshotAndFirstSightingNameTheCommunity(t *testing.T) {
	t.Parallel()
	parent, member, _ := community()
	r, inputs := reporter(fakePostingDirectory{groups: []*types.GroupInfo{parent, member}})
	if err := r.Snapshot(context.Background()); err != nil {
		t.Fatal(err)
	}
	var found *wuphfpb.ChatUpdate
	for _, update := range (<-inputs).GetChatUpdate().GetUpdates() {
		if update.GetChatId() == "1@g.us" {
			found = update
		}
	}
	if found.GetCommunity().GetChatId() != "9@g.us" || found.GetTitle() != "[G] Neighbours › Garden" {
		t.Fatalf("snapshot: %v", found)
	}

	groups := fakeGroups{"9@g.us": parent, "1@g.us": member}
	p := profiler(fakePictures{}, fetchBytes(nil, nil), &uploadingBroker{}, groups)
	sighting := p.firstSighting(context.Background(), member.JID, "1@g.us", "[G] Neighbours › Garden")
	if sighting.GetCommunity().GetChatId() != "9@g.us" || sighting.GetCommunity().GetTitle() != "Neighbours" {
		t.Fatalf("first sighting: %v", sighting)
	}
}

func TestTheDirectoryListsCommunitiesAndTheirGroups(t *testing.T) {
	t.Parallel()
	parent, member, _ := community()
	chats := directoryChats("whatsapp-main", []*types.GroupInfo{parent, member}, nil, nil, func(jid types.JID) types.JID { return jid })
	got := map[string]chatdir.Chat{}
	for _, chat := range chats {
		got[chat.Chat] = chat
	}
	if c := got["whatsapp-main:9@g.us"]; c.Kind != chatdir.Community || c.Title != "Neighbours" {
		t.Errorf("community: %+v", c)
	}
	if c := got["whatsapp-main:1@g.us"]; c.Kind != chatdir.Group || c.Title != "Neighbours › Garden" {
		t.Errorf("member: %+v", c)
	}
}
