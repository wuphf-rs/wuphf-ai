package whatsappconnector

import (
	"context"
	"testing"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

type fakeDirectory struct {
	info *types.GroupInfo
	err  error
}

func (f fakeDirectory) OwnPN() types.JID  { return types.NewJID("911", types.DefaultUserServer) }
func (f fakeDirectory) OwnLID() types.JID { return types.NewJID("55", types.HiddenUserServer) }
func (f fakeDirectory) GetGroupInfo(context.Context, types.JID) (*types.GroupInfo, error) {
	return f.info, f.err
}

func groupWith(announce bool, me *types.GroupParticipant) *types.GroupInfo {
	info := &types.GroupInfo{GroupAnnounce: types.GroupAnnounce{IsAnnounce: announce}}
	info.Participants = []types.GroupParticipant{{JID: types.NewJID("999", types.DefaultUserServer)}}
	if me != nil {
		info.Participants = append(info.Participants, *me)
	}
	return info
}

func TestAnAnnounceOnlyGroupTakesAnAdminToPost(t *testing.T) {
	me := &types.GroupParticipant{JID: types.NewJID("55", types.HiddenUserServer)}
	facts := probeChat(context.Background(), fakeDirectory{info: groupWith(true, me)}, "1203@g.us")
	if facts.GetMember() != wuphfpb.Fact_FACT_YES || facts.GetCanSend() != wuphfpb.Fact_FACT_NO || facts.GetDetail() == "" {
		t.Fatalf("a member of an announce-only group: %v", facts)
	}
	me.IsAdmin = true
	facts = probeChat(context.Background(), fakeDirectory{info: groupWith(true, me)}, "1203@g.us")
	if facts.GetCanSend() != wuphfpb.Fact_FACT_YES || facts.GetCanDeleteOthers() != wuphfpb.Fact_FACT_YES {
		t.Fatalf("an admin there: %v", facts)
	}
}

func TestOutOfTheGroupOrGone(t *testing.T) {
	removed := probeChat(context.Background(), fakeDirectory{info: groupWith(false, nil)}, "1203@g.us")
	if removed.GetMember() != wuphfpb.Fact_FACT_NO || removed.GetCanSend() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("not among the participants: %v", removed)
	}
	forbidden := probeChat(context.Background(), fakeDirectory{err: whatsmeow.ErrNotInGroup}, "1203@g.us")
	if forbidden.GetMember() != wuphfpb.Fact_FACT_NO || forbidden.GetExists() != wuphfpb.Fact_FACT_YES {
		t.Fatalf("not in the group: %v", forbidden)
	}
	gone := probeChat(context.Background(), fakeDirectory{err: whatsmeow.ErrGroupNotFound}, "1203@g.us")
	if gone.GetExists() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("a deleted group: %v", gone)
	}
}

func TestADirectChatIsAlwaysWritable(t *testing.T) {
	facts := probeChat(context.Background(), fakeDirectory{}, "911@s.whatsapp.net")
	if facts.GetCanSend() != wuphfpb.Fact_FACT_YES || facts.GetForum() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("facts = %v", facts)
	}
}
