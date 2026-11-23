package whatsappconnector

import (
	"context"
	"errors"
	"testing"
	"time"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types"
)

// stubGroups answers group lookups from a fixed table and counts them.
type stubGroups struct {
	groups  map[types.JID]*types.GroupInfo
	lookups int
}

func (s *stubGroups) GetGroupInfo(_ context.Context, jid types.JID) (*types.GroupInfo, error) {
	s.lookups++
	if info, ok := s.groups[jid]; ok {
		return info, nil
	}
	return nil, errors.New("not found")
}

func TestTimerSourcePrefersTheDispatchedTimer(t *testing.T) {
	t.Parallel()
	groups := &stubGroups{}
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	source := newTimerSource(groups, inputs, context.Background())
	group := types.NewJID("123", types.GroupServer)
	dispatched := &wuphfpb.DisappearingTimer{Seconds: 86400}
	if got := source.For(context.Background(), group.String(), group, dispatched); got != dispatched {
		t.Fatalf("For() = %v, want the dispatched timer", got)
	}
	if groups.lookups != 0 || len(inputs) != 0 {
		t.Fatalf("a dispatched timer caused %d lookups and %d frames", groups.lookups, len(inputs))
	}
}

func TestTimerSourceLooksUpAGroupAndReportsIt(t *testing.T) {
	t.Parallel()
	on := types.NewJID("1", types.GroupServer)
	off := types.NewJID("2", types.GroupServer)
	groups := &stubGroups{groups: map[types.JID]*types.GroupInfo{
		on:  {GroupEphemeral: types.GroupEphemeral{IsEphemeral: true, DisappearingTimer: 604800}},
		off: {GroupEphemeral: types.GroupEphemeral{IsEphemeral: false, DisappearingTimer: 604800}},
	}}
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	source := newTimerSource(groups, inputs, context.Background())

	for _, c := range []struct {
		group types.JID
		want  uint32
	}{{on, 604800}, {off, 0}} {
		got := source.For(context.Background(), c.group.String(), c.group, nil)
		if got == nil || got.GetSeconds() != c.want || got.SettingTimestamp != nil {
			t.Fatalf("%s: For() = %v, want %d without a setting timestamp", c.group, got, c.want)
		}
		updates := (<-inputs).GetChatUpdate().GetUpdates()
		if len(updates) != 1 || updates[0].GetChatId() != c.group.String() || updates[0].GetDisappearing().GetSeconds() != c.want {
			t.Fatalf("%s: reported %v, want the group's timer", c.group, updates)
		}
	}
}

func TestTimerSourceWarnsOncePerChatUntilKnown(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	source := newTimerSource(&stubGroups{}, inputs, context.Background())
	dm := types.NewJID("1555", types.DefaultUserServer)

	notices := func() int {
		count := 0
		for {
			select {
			case frame := <-inputs:
				notice := frame.GetOwnerNotice()
				if notice.GetKind() != wuphfpb.NoticeKind_NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN || notice.GetDetail() != "chat "+dm.String() {
					t.Fatalf("frame = %v, want an unknown-timer notice naming the chat", frame)
				}
				count++
			default:
				return count
			}
		}
	}

	if got := source.For(context.Background(), dm.String(), dm, nil); got != nil {
		t.Fatalf("For() = %v, want nil for an unknown DM", got)
	}
	if n := notices(); n != 1 {
		t.Fatalf("first unknown send raised %d notices, want 1", n)
	}
	source.For(context.Background(), dm.String(), dm, nil)
	if n := notices(); n != 0 {
		t.Fatalf("second unknown send raised %d notices, want 0", n)
	}
	source.For(context.Background(), dm.String(), dm, &wuphfpb.DisappearingTimer{Seconds: 0})
	source.For(context.Background(), dm.String(), dm, nil)
	if n := notices(); n != 1 {
		t.Fatalf("unknown again after a known timer raised %d notices, want 1", n)
	}
}

func TestTimerSourceNeverBlocksOnAFullInputChannel(t *testing.T) {
	t.Parallel()
	life, stop := context.WithCancel(context.Background())
	defer stop()
	// Unbuffered and unread: any blocking send would hang For.
	source := newTimerSource(&stubGroups{}, make(chan *wuphfpb.ClientFrame), life)
	dm := types.NewJID("1555", types.DefaultUserServer)
	done := make(chan struct{})
	go func() {
		source.For(context.Background(), dm.String(), dm, nil)
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("For blocked on a full input channel")
	}
}
