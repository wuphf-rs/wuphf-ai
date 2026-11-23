package whatsappconnector

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	waHistorySync "go.mau.fi/whatsmeow/proto/waHistorySync"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

// recordingNamer names chats from fixed titles and people by their user part
// or a fixed name, and records which cached titles were dropped.
type recordingNamer struct {
	titles    map[string]string
	people    map[string]string
	forgotten []string
}

func (r *recordingNamer) ChatTitle(_ context.Context, chat types.JID) string {
	return r.titles[chat.String()]
}

func (r *recordingNamer) PersonName(_ context.Context, jid types.JID, pushHint string) string {
	if name, ok := r.people[jid.User]; ok {
		return name
	}
	if pushHint != "" {
		return pushHint
	}
	return jid.User
}

func (r *recordingNamer) Forget(chat types.JID) {
	r.forgotten = append(r.forgotten, chat.String())
}

type fakePictures struct {
	info *types.ProfilePictureInfo
	err  error
}

func (f fakePictures) GetProfilePictureInfo(context.Context, types.JID, *whatsmeow.GetProfilePictureParams) (*types.ProfilePictureInfo, error) {
	return f.info, f.err
}

type uploadingBroker struct {
	uploads  [][]byte
	metadata []*wuphfpb.MediaMetadata
}

func (b *uploadingBroker) Upload(_ context.Context, metadata *wuphfpb.MediaMetadata, bytes []byte) (*wuphfpb.Attachment, error) {
	b.uploads = append(b.uploads, bytes)
	b.metadata = append(b.metadata, metadata)
	return &wuphfpb.Attachment{MediaId: "media-1"}, nil
}

func (*uploadingBroker) Download(context.Context, string) ([]byte, error) { return nil, nil }

type fakeGroups map[string]*types.GroupInfo

func (f fakeGroups) GroupInfo(_ context.Context, group types.JID) *types.GroupInfo {
	return f[group.String()]
}

func fetchBytes(bytes []byte, err error) fetchURL {
	return func(context.Context, string) ([]byte, error) { return bytes, err }
}

func profiler(pictures pictureSource, fetch fetchURL, broker MediaBroker, groups groupInfoSource) *chatProfiler {
	return newChatProfiler(pictures, fetch, broker, groups, zerolog.Nop())
}

func TestHandleGroupInfoReportsNameTopicAndMembers(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	namer := &recordingNamer{people: map[string]string{"1": "Bob", "2": "Alice"}}
	normalizer := NewNormalizer(inputs, MediaCodec{}, namer, nil, nil, nil)
	group := types.NewJID("123", types.GroupServer)
	bob := types.NewJID("1", types.DefaultUserServer)
	alice := types.NewJID("2", types.DefaultUserServer)
	carol := types.NewJID("3", types.DefaultUserServer)
	handle := func(evt *events.GroupInfo) *wuphfpb.ChatUpdateEvent {
		t.Helper()
		evt.JID = group
		if err := normalizer.HandleGroupInfo(context.Background(), evt); err != nil {
			t.Fatalf("HandleGroupInfo() error = %v", err)
		}
		return nextChatUpdate(t, inputs)
	}

	event := handle(&events.GroupInfo{
		Name:  &types.GroupName{Name: "Family"},
		Topic: &types.GroupTopic{Topic: "Weekend plans"},
	})
	update := event.GetUpdates()[0]
	if event.GetSnapshot() || update.GetChatId() != "123@g.us" || update.GetTitle() != "[G] Family" ||
		update.GetDescription() != "Weekend plans" {
		t.Fatalf("event = %v, want a live tagged title and the topic", event)
	}
	if len(namer.forgotten) != 1 || namer.forgotten[0] != "123@g.us" {
		t.Fatalf("forgotten = %v, want the group's cached title dropped", namer.forgotten)
	}

	update = handle(&events.GroupInfo{Topic: &types.GroupTopic{Topic: "old", TopicDeleted: true}}).GetUpdates()[0]
	if update.Description == nil || update.GetDescription() != "" {
		t.Fatalf("a deleted topic reports an empty description, got %v", update)
	}

	// With an actor: the admin's id and name ride on every change.
	update = handle(&events.GroupInfo{
		Sender:  &alice,
		Join:    []types.JID{bob},
		Leave:   []types.JID{carol},
		Promote: []types.JID{bob},
		Demote:  []types.JID{carol},
	}).GetUpdates()[0]
	kinds := []wuphfpb.ParticipantChange_Kind{
		wuphfpb.ParticipantChange_KIND_JOINED, wuphfpb.ParticipantChange_KIND_LEFT,
		wuphfpb.ParticipantChange_KIND_PROMOTED, wuphfpb.ParticipantChange_KIND_DEMOTED,
	}
	changes := update.GetParticipantChanges()
	if len(changes) != 4 {
		t.Fatalf("changes = %v, want 4", changes)
	}
	for i, change := range changes {
		if change.GetKind() != kinds[i] || change.GetActor() != alice.String() || change.GetActorName() != "Alice" {
			t.Fatalf("change %d = %v, want %v by Alice", i, change, kinds[i])
		}
	}
	if changes[0].GetParticipant() != bob.String() || changes[0].GetName() != "Bob" {
		t.Fatalf("join = %v, want Bob named from the namer", changes[0])
	}
	if changes[1].GetName() != "3" {
		t.Fatalf("an unknown member falls back to the number, got %v", changes[1])
	}

	// An invite-link join has no actor; a self-leave names the member.
	update = handle(&events.GroupInfo{Join: []types.JID{bob}}).GetUpdates()[0]
	if change := update.GetParticipantChanges()[0]; change.Actor != nil || change.ActorName != nil {
		t.Fatalf("an actorless join reports no actor, got %v", change)
	}
	update = handle(&events.GroupInfo{Sender: &bob, Leave: []types.JID{bob}}).GetUpdates()[0]
	if change := update.GetParticipantChanges()[0]; change.GetActor() != change.GetParticipant() {
		t.Fatalf("a self-leave reports the member as the actor, got %v", change)
	}
}

func TestPictureEventUploadsTheAvatar(t *testing.T) {
	t.Parallel()
	group := types.NewJID("123", types.GroupServer)
	picture := &types.ProfilePictureInfo{URL: "https://cdn/pic", ID: "pic-2"}
	handle := func(profiles *chatProfiler, remove bool) *wuphfpb.ClientFrame {
		t.Helper()
		inputs := make(chan *wuphfpb.ClientFrame, 2)
		normalizer := NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil).WithProfiles(profiles)
		if err := normalizer.HandlePicture(context.Background(), &events.Picture{JID: group, Remove: remove}); err != nil {
			t.Fatalf("HandlePicture() error = %v", err)
		}
		select {
		case frame := <-inputs:
			return frame
		default:
			return nil
		}
	}

	broker := &uploadingBroker{}
	frame := handle(profiler(fakePictures{info: picture}, fetchBytes([]byte("jpeg"), nil), broker, nil), false)
	avatar := frame.GetChatUpdate().GetUpdates()[0].GetAvatar()
	if avatar.GetId() != "pic-2" || avatar.GetMediaId() != "media-1" {
		t.Fatalf("avatar = %v, want pic-2 uploaded as media-1", avatar)
	}
	if string(broker.uploads[0]) != "jpeg" || broker.metadata[0].GetKind() != wuphfpb.Attachment_KIND_IMAGE ||
		broker.metadata[0].GetMimeType() != "image/jpeg" || broker.metadata[0].GetSizeBytes() != 4 {
		t.Fatalf("uploaded %q as %v, want the downloaded JPEG", broker.uploads[0], broker.metadata[0])
	}

	frame = handle(profiler(fakePictures{info: picture}, fetchBytes(nil, nil), &uploadingBroker{}, nil), true)
	if avatar := frame.GetChatUpdate().GetUpdates()[0].GetAvatar(); avatar == nil || avatar.GetId() != "" {
		t.Fatalf("a removal reports an empty id, got %v", avatar)
	}

	if frame := handle(profiler(fakePictures{info: picture}, fetchBytes(nil, errors.New("timeout")), &uploadingBroker{}, nil), false); frame != nil {
		t.Fatalf("a failed download reports nothing, got %v", frame)
	}

	broker = &uploadingBroker{}
	frame = handle(profiler(fakePictures{err: whatsmeow.ErrProfilePictureNotSet}, fetchBytes(nil, nil), broker, nil), false)
	if avatar := frame.GetChatUpdate().GetUpdates()[0].GetAvatar(); avatar.GetId() != "" || len(broker.uploads) != 0 {
		t.Fatalf("a chat without a picture reports an empty id, got %v", avatar)
	}
}

func TestContactEventsRetitleTheDM(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	person := types.NewJID("15550001111", types.DefaultUserServer)
	namer := &recordingNamer{titles: map[string]string{person.String(): "[P] Bob"}}
	normalizer := NewNormalizer(inputs, MediaCodec{}, namer, nil, nil, nil)

	if err := normalizer.HandleNameChange(context.Background(), person, false); err != nil {
		t.Fatal(err)
	}
	event := nextChatUpdate(t, inputs)
	if event.GetSnapshot() || event.GetUpdates()[0].GetChatId() != person.String() || event.GetUpdates()[0].GetTitle() != "[P] Bob" {
		t.Fatalf("event = %v, want a live retitle of the DM", event)
	}
	if len(namer.forgotten) != 1 || namer.forgotten[0] != person.String() {
		t.Fatalf("forgotten = %v, want the DM's cached title dropped first", namer.forgotten)
	}

	if err := normalizer.HandleNameChange(context.Background(), person, true); err != nil {
		t.Fatal(err)
	}
	if !nextChatUpdate(t, inputs).GetSnapshot() {
		t.Fatal("a full-sync contact change is a snapshot")
	}

	// A group JID names no person.
	if err := normalizer.HandleNameChange(context.Background(), types.NewJID("123", types.GroupServer), false); err != nil {
		t.Fatal(err)
	}
	select {
	case frame := <-inputs:
		t.Fatalf("a group JID produced %v", frame)
	default:
	}
}

func TestHistorySyncReportsTitlesAsSnapshot(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 2)
	person := types.NewJID("15550001111", types.DefaultUserServer)
	namer := &recordingNamer{titles: map[string]string{person.String(): "[P] Bob"}}
	normalizer := NewNormalizer(inputs, MediaCodec{}, namer, nil, nil, nil)

	if err := normalizer.HandleHistorySync(context.Background(), &events.HistorySync{Data: &waHistorySync.HistorySync{
		SyncType: waHistorySync.HistorySync_INITIAL_BOOTSTRAP.Enum(),
		Conversations: []*waHistorySync.Conversation{
			{ID: proto.String("123@g.us"), Name: proto.String("Family")},
			{ID: proto.String("456@g.us")},
			{ID: proto.String(person.String())},
		},
	}}); err != nil {
		t.Fatal(err)
	}
	event := nextChatUpdate(t, inputs)
	if !event.GetSnapshot() {
		t.Fatal("a history sync is a snapshot")
	}
	updates := event.GetUpdates()
	if updates[0].GetTitle() != "[G] Family" || updates[1].Title != nil || updates[2].GetTitle() != "[P] Bob" {
		t.Fatalf("titles = %q %v %q, want the tagged group name, none, the DM's title",
			updates[0].GetTitle(), updates[1].Title, updates[2].GetTitle())
	}
}

func TestFirstSightingSendsOneSnapshotBeforeTheMessage(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	group := types.NewJID("123", types.GroupServer)
	namer := &recordingNamer{titles: map[string]string{group.String(): "[G] Family"}}
	groups := fakeGroups{group.String(): {
		GroupTopic:   types.GroupTopic{Topic: "Weekend plans"},
		Participants: make([]types.GroupParticipant, 12),
	}}
	broker := &uploadingBroker{}
	profiles := profiler(fakePictures{info: &types.ProfilePictureInfo{URL: "u", ID: "pic-1"}}, fetchBytes([]byte("jpeg"), nil), broker, groups)
	normalizer := NewNormalizer(inputs, MediaCodec{}, namer, nil, nil, nil).WithProfiles(profiles)
	message := func(id string) *events.Message {
		return &events.Message{
			Info: types.MessageInfo{
				MessageSource: types.MessageSource{
					Chat:    group,
					Sender:  types.NewJID("9988", types.DefaultUserServer),
					IsGroup: true,
				},
				ID:        types.MessageID(id),
				Timestamp: time.Unix(1700000000, 0),
			},
			Message: &waE2E.Message{Conversation: proto.String("hi")},
		}
	}

	if err := normalizer.Handle(context.Background(), message("wa-1")); err != nil {
		t.Fatal(err)
	}
	snapshot := (<-inputs).GetChatUpdate()
	if snapshot == nil || !snapshot.GetSnapshot() {
		t.Fatalf("the first frame is not a snapshot: %v", snapshot)
	}
	update := snapshot.GetUpdates()[0]
	if update.GetChatId() != "123@g.us" || update.GetTitle() != "[G] Family" || update.GetDescription() != "Weekend plans" ||
		update.GetParticipantCount() != 12 || update.GetAvatar().GetMediaId() != "media-1" {
		t.Fatalf("snapshot = %v, want title, topic, 12 members and the picture", update)
	}
	if create := (<-inputs).GetMessageCreate(); create == nil || create.GetPlatformMessageId() != "wa-1" {
		t.Fatalf("the message does not follow the snapshot: %v", create)
	}

	if err := normalizer.Handle(context.Background(), message("wa-2")); err != nil {
		t.Fatal(err)
	}
	if create := (<-inputs).GetMessageCreate(); create == nil || create.GetPlatformMessageId() != "wa-2" {
		t.Fatalf("a second message sends no snapshot, got %v", create)
	}
	if len(broker.uploads) != 1 {
		t.Fatalf("the picture was uploaded %d times, want once per process", len(broker.uploads))
	}
}

func TestRenderMentions(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		text  string
		names map[string]string
		want  string
	}{
		{"hi @15550001111!", map[string]string{"15550001111": "Bob"}, "hi @Bob!"},
		{"@1 and @2", map[string]string{"1": "Bob", "2": "Alice"}, "@Bob and @Alice"},
		{"hi @15550001111", map[string]string{"15550001111": "15550001111"}, "hi @15550001111"},
		{"hi @15550001111", map[string]string{}, "hi @15550001111"},
		{"no mention", map[string]string{"1": "Bob"}, "no mention"},
	} {
		if got := renderMentions(c.text, c.names); got != c.want {
			t.Errorf("renderMentions(%q) = %q, want %q", c.text, got, c.want)
		}
	}
}

func TestNormalizeRendersMentionNames(t *testing.T) {
	t.Parallel()
	namer := &recordingNamer{people: map[string]string{"15550001111": "Bob"}}
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, namer, nil, nil, nil)
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:    types.NewJID("123", types.GroupServer),
				Sender:  types.NewJID("9988", types.DefaultUserServer),
				IsGroup: true,
			},
			ID: "wa-1",
		},
		Message: &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
			Text:        proto.String("ping @15550001111"),
			ContextInfo: &waE2E.ContextInfo{MentionedJID: []string{"15550001111@s.whatsapp.net"}},
		}},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() = %v, %v", ok, err)
	}
	if event.GetText() != "ping @Bob" {
		t.Fatalf("text = %q, want the mention named", event.GetText())
	}
}
