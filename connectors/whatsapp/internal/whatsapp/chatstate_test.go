package whatsappconnector

import (
	"context"
	"testing"
	"time"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/appstate"
	waHistorySync "go.mau.fi/whatsmeow/proto/waHistorySync"
	"go.mau.fi/whatsmeow/proto/waSyncAction"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

func TestMuteFromWhatsApp(t *testing.T) {
	now := time.Unix(1_800_000_000, 0)
	for _, c := range []struct {
		name   string
		muted  bool
		end    int64
		expect int64
	}{
		{"not muted", false, 1_900_000_000_000, 0},
		{"forever", true, -1, -1},
		{"muted without an end", true, 0, -1},
		{"until", true, 1_900_000_000_000, 1_900_000_000},
		{"over", true, 1_700_000_000_000, 0},
	} {
		if got := muteFromWhatsApp(c.muted, c.end, now); got != c.expect {
			t.Errorf("%s: %d, want %d", c.name, got, c.expect)
		}
	}
}

func TestMuteFromHistoryReadsMillisecondsOrSeconds(t *testing.T) {
	now := time.Unix(1_800_000_000, 0)
	for end, want := range map[uint64]int64{
		0:                 0,
		1_900_000_000:     1_900_000_000,
		1_900_000_000_000: 1_900_000_000,
		1_700_000_000_000: 0,
		1_700_000_000:     0,
	} {
		if got := muteFromHistory(end, now); got != want {
			t.Errorf("muteFromHistory(%d) = %d, want %d", end, got, want)
		}
	}
}

func TestChatStateEventsBecomeChatUpdates(t *testing.T) {
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil)
	ctx := context.Background()
	group := types.NewJID("123", types.GroupServer)

	if err := normalizer.HandlePin(ctx, &events.Pin{JID: group, Action: &waSyncAction.PinAction{Pinned: proto.Bool(true)}}); err != nil {
		t.Fatal(err)
	}
	event := nextChatUpdate(t, inputs)
	if event.GetSnapshot() || event.GetUpdates()[0].GetChatId() != "123@g.us" || !event.GetUpdates()[0].GetPinned() {
		t.Fatalf("pin = %v", event)
	}

	if err := normalizer.HandleMute(ctx, &events.Mute{JID: group, Action: &waSyncAction.MuteAction{
		Muted: proto.Bool(true), MuteEndTimestamp: proto.Int64(-1),
	}}); err != nil {
		t.Fatal(err)
	}
	if mute := nextChatUpdate(t, inputs).GetUpdates()[0].GetMute(); mute.GetMutedUntil() != -1 {
		t.Fatalf("mute = %v, want forever", mute)
	}

	if err := normalizer.HandleArchive(ctx, &events.Archive{JID: group, FromFullSync: true, Action: &waSyncAction.ArchiveChatAction{
		Archived: proto.Bool(true),
	}}); err != nil {
		t.Fatal(err)
	}
	event = nextChatUpdate(t, inputs)
	if !event.GetSnapshot() || !event.GetUpdates()[0].GetArchived() {
		t.Fatalf("full-sync archive = %v, want a snapshot", event)
	}
}

func TestHistorySyncReportsChatState(t *testing.T) {
	inputs := make(chan *wuphfpb.ClientFrame, 2)
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil)
	future := uint64(time.Now().Add(24 * time.Hour).UnixMilli())
	if err := normalizer.HandleHistorySync(context.Background(), &events.HistorySync{Data: &waHistorySync.HistorySync{
		SyncType: waHistorySync.HistorySync_INITIAL_BOOTSTRAP.Enum(),
		Conversations: []*waHistorySync.Conversation{
			{ID: proto.String("123@g.us"), Pinned: proto.Uint32(3), Archived: proto.Bool(true), MuteEndTime: proto.Uint64(future)},
			{ID: proto.String("456@g.us")},
		},
	}}); err != nil {
		t.Fatal(err)
	}
	updates := nextChatUpdate(t, inputs).GetUpdates()
	if !updates[0].GetPinned() || !updates[0].GetArchived() || updates[0].GetMute().GetMutedUntil() != int64(future/1000) {
		t.Fatalf("first = %v, want pinned, archived, muted until the end in seconds", updates[0])
	}
	if updates[1].Pinned == nil || updates[1].GetPinned() || updates[1].GetArchived() || updates[1].GetMute().GetMutedUntil() != 0 {
		t.Fatalf("second = %v, want the plain baseline", updates[1])
	}
}

func muteEnd(t *testing.T, patch appstate.PatchInfo) (bool, *int64) {
	t.Helper()
	action := patch.Mutations[0].Value.GetMuteAction()
	if action == nil {
		t.Fatalf("patch %v is no mute", patch)
	}
	return action.GetMuted(), action.MuteEndTimestamp
}

func TestApplyChatStateSendsOnePatchPerField(t *testing.T) {
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, stubConverter{}, staticAddressing{}, liveRendering(config.RenderingOptions{}))
	until := time.Now().Add(time.Hour).Unix()
	chatID, err := sender.ApplyChatState(context.Background(), &wuphfpb.ChatStateDispatch{
		TargetChatId: "123@g.us",
		Pinned:       proto.Bool(true),
		Mute:         &wuphfpb.MuteState{MutedUntil: until},
		Archived:     proto.Bool(false),
	})
	if err != nil || chatID != "123@g.us" {
		t.Fatalf("ApplyChatState() = %q, %v", chatID, err)
	}
	if len(client.patches) != 3 {
		t.Fatalf("patches = %d, want one per field", len(client.patches))
	}
	if !client.patches[0].Mutations[0].Value.GetPinAction().GetPinned() {
		t.Fatalf("first patch %v is not a pin", client.patches[0])
	}
	muted, end := muteEnd(t, client.patches[1])
	if !muted || end == nil || *end != until*1000 {
		t.Fatalf("mute end = %v, want exactly %d ms", end, until*1000)
	}
	if archive := client.patches[2].Mutations[0].Value.GetArchiveChatAction(); archive == nil || archive.GetArchived() {
		t.Fatalf("third patch %v is not an unarchive", client.patches[2])
	}
}

func TestMutePatchForeverAndUnmute(t *testing.T) {
	chat := types.NewJID("123", types.GroupServer)
	now := time.Unix(1_800_000_000, 0)
	muted, end := muteEnd(t, mutePatch(chat, -1, now))
	if !muted || end == nil || *end != -1 {
		t.Fatalf("forever = %v %v, want muted with whatsmeow's -1", muted, end)
	}
	for _, until := range []int64{0, 1_700_000_000} {
		if muted, _ := muteEnd(t, mutePatch(chat, until, now)); muted {
			t.Fatalf("mutePatch(%d) mutes, want an unmute", until)
		}
	}
}
