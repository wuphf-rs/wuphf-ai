package whatsappconnector

import (
	"context"
	"errors"
	"testing"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

var (
	peerPN  = types.NewJID("15550002222", types.DefaultUserServer)
	peerLID = types.NewJID("77776666", types.HiddenUserServer)
	ownPN   = types.NewJID("15550001111", types.DefaultUserServer)
	ownLID  = types.NewJID("88887777", types.HiddenUserServer)
	group   = types.NewJID("120363000000000000", types.GroupServer)
)

// fakeAccount answers group metadata from a table, counting lookups.
type fakeAccount struct {
	migrated bool
	groups   map[types.JID]types.AddressingMode
	lookups  int
}

func (a *fakeAccount) OwnPN() types.JID  { return ownPN }
func (a *fakeAccount) OwnLID() types.JID { return ownLID }
func (a *fakeAccount) LIDMigrated() bool { return a.migrated }

func (a *fakeAccount) GroupMode(_ context.Context, jid types.JID) (types.AddressingMode, error) {
	a.lookups++
	mode, ok := a.groups[jid]
	if !ok {
		return "", errors.New("group metadata unavailable")
	}
	return mode, nil
}

func book(account *fakeAccount) *addressBook {
	return newAddressBook(fakeIdentity{lids: map[string]types.JID{peerPN.String(): peerLID}}, account)
}

func TestGroupModeComesFromItsMessages(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	account := &fakeAccount{}
	addresses := book(account)

	addresses.Observe(group, types.MessageSource{Chat: group, IsGroup: true, AddressingMode: types.AddressingModeLID})
	if mode := addresses.Mode(ctx, group); mode != types.AddressingModeLID {
		t.Fatalf("mode = %q, want lid", mode)
	}
	// A group that migrates says so on its next message.
	addresses.Observe(group, types.MessageSource{Chat: group, IsGroup: true})
	if mode := addresses.Mode(ctx, group); mode != types.AddressingModePN {
		t.Fatalf("mode = %q, want pn", mode)
	}
	if account.lookups != 0 {
		t.Fatalf("looked up group metadata %d times, want 0 for an observed group", account.lookups)
	}
}

func TestGroupModeFallsBackToWhatsAppOnce(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	account := &fakeAccount{groups: map[types.JID]types.AddressingMode{group: types.AddressingModeLID}}
	addresses := book(account)
	for range 2 {
		if mode := addresses.Mode(ctx, group); mode != types.AddressingModeLID {
			t.Fatalf("mode = %q, want lid", mode)
		}
	}
	if account.lookups != 1 {
		t.Fatalf("looked up group metadata %d times, want 1 (cached)", account.lookups)
	}
}

func TestUnknownGroupModeKeepsAddresses(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	addresses := book(&fakeAccount{})
	mode := addresses.Mode(ctx, group)
	if mode != "" {
		t.Fatalf("mode = %q, want unknown", mode)
	}
	if got := addresses.InMode(ctx, peerLID, mode); got != peerLID {
		t.Fatalf("InMode() = %s, want the address unchanged", got)
	}
}

func TestDirectChatMode(t *testing.T) {
	t.Parallel()
	ctx := context.Background()

	addresses := book(&fakeAccount{})
	// Before any message: the form of the address itself.
	if mode := addresses.Mode(ctx, peerLID); mode != types.AddressingModeLID {
		t.Fatalf("unobserved LID chat mode = %q, want lid", mode)
	}
	// A chat that arrives by phone number is answered by phone number, though
	// dispatches name it by its canonical LID.
	addresses.Observe(peerLID, types.MessageSource{Chat: peerPN})
	if mode := addresses.Mode(ctx, peerLID); mode != types.AddressingModePN {
		t.Fatalf("phone-addressed chat mode = %q, want pn", mode)
	}

	// A LID-migrated account sends every direct message to the LID.
	migrated := book(&fakeAccount{migrated: true})
	migrated.Observe(peerLID, types.MessageSource{Chat: peerPN})
	if mode := migrated.Mode(ctx, peerLID); mode != types.AddressingModeLID {
		t.Fatalf("migrated account mode = %q, want lid", mode)
	}
}

func TestInModeTranslatesKnownCounterparts(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	addresses := book(&fakeAccount{})
	unknown := types.NewJID("99990000", types.HiddenUserServer)
	for _, test := range []struct {
		jid  types.JID
		mode types.AddressingMode
		want types.JID
	}{
		{peerLID, types.AddressingModePN, peerPN},
		{peerPN, types.AddressingModeLID, peerLID},
		{peerLID, types.AddressingModeLID, peerLID},
		{types.JID{User: peerPN.User, Server: types.DefaultUserServer, Device: 3}, types.AddressingModePN, peerPN},
		{unknown, types.AddressingModePN, unknown},
		{group, types.AddressingModePN, group},
	} {
		if got := addresses.InMode(ctx, test.jid, test.mode); got != test.want {
			t.Errorf("InMode(%s, %q) = %s, want %s", test.jid, test.mode, got, test.want)
		}
	}
	if got := addresses.Own(types.AddressingModeLID); got != ownLID {
		t.Errorf("Own(lid) = %s, want %s", got, ownLID)
	}
	if got := addresses.Own(types.AddressingModePN); got != ownPN {
		t.Errorf("Own(pn) = %s, want %s", got, ownPN)
	}
}

// replyIn sends a reply quoting reply into chat and returns what WhatsApp got.
func replyIn(t *testing.T, addressing Addressing, chat types.JID, reply *wuphfpb.ReplyContext) *stubSendClient {
	t.Helper()
	client := &stubSendClient{}
	sender := NewSender(client, &stubBroker{}, stubConverter{}, addressing, config.StickerMetadata{}, config.RenderingOptions{})
	text := "reply"
	reply.PlatformMessageId = "parent"
	if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: chat.String(),
		Text:         &text,
		ReplyContext: reply,
	}); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	return client
}

func TestReplyQuotesAuthorInTheChatsForm(t *testing.T) {
	t.Parallel()
	counterparts := map[types.JID]types.JID{peerLID: peerPN, peerPN: peerLID}
	participant := peerLID.String()
	for _, test := range []struct {
		name  string
		mode  types.AddressingMode
		reply *wuphfpb.ReplyContext
		want  types.JID
	}{
		{"phone group quotes the participant by phone", types.AddressingModePN,
			&wuphfpb.ReplyContext{Sender: group.String(), Participant: &participant}, peerPN},
		{"LID group quotes the participant by LID", types.AddressingModeLID,
			&wuphfpb.ReplyContext{Sender: group.String(), Participant: &participant}, peerLID},
		{"own copy in a LID group quotes our LID", types.AddressingModeLID,
			&wuphfpb.ReplyContext{AuthoredByBridge: true}, ownLID},
		{"own copy in a phone group quotes our phone JID", types.AddressingModePN,
			&wuphfpb.ReplyContext{AuthoredByBridge: true}, ownPN},
	} {
		t.Run(test.name, func(t *testing.T) {
			addressing := staticAddressing{mode: test.mode, ownPN: ownPN, ownLID: ownLID, counterparts: counterparts}
			client := replyIn(t, addressing, group, test.reply)
			got := client.sent.GetExtendedTextMessage().GetContextInfo().GetParticipant()
			if got != test.want.String() {
				t.Fatalf("quoted participant = %q, want %q", got, test.want)
			}
			if client.sentTo != group {
				t.Fatalf("sent to %s, want the group unchanged", client.sentTo)
			}
		})
	}
}

func TestDirectChatIsAddressedInItsForm(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	addresses := book(&fakeAccount{})
	// The peer wrote by phone number; the core names the chat by its LID.
	addresses.Observe(peerLID, types.MessageSource{Chat: peerPN, Sender: peerPN})

	client := replyIn(t, addresses, peerLID, &wuphfpb.ReplyContext{Sender: peerLID.String()})
	if client.sentTo != peerPN {
		t.Fatalf("sent to %s, want the phone JID the chat arrived on", client.sentTo)
	}
	if got := client.sent.GetExtendedTextMessage().GetContextInfo().GetParticipant(); got != peerPN.String() {
		t.Fatalf("quoted participant = %q, want %q", got, peerPN)
	}

	sender := NewSender(client, &stubBroker{}, stubConverter{}, addresses, config.StickerMetadata{}, config.RenderingOptions{})
	if _, err := sender.EditMessage(ctx, &wuphfpb.MessageEditDispatch{TargetChatId: peerLID.String(), TargetPlatformMessageId: "m", Text: proto.String("x")}); err != nil {
		t.Fatal(err)
	}
	if client.sentTo != peerPN {
		t.Fatalf("edit sent to %s, want %s", client.sentTo, peerPN)
	}
	if _, err := sender.DeleteMessage(ctx, &wuphfpb.MessageDeleteDispatch{TargetChatId: peerLID.String(), TargetPlatformMessageId: "m"}); err != nil {
		t.Fatal(err)
	}
	if client.sentTo != peerPN {
		t.Fatalf("delete sent to %s, want %s", client.sentTo, peerPN)
	}
}

// recordingAddresses records what the normalizer observes.
type recordingAddresses struct {
	chats   []types.JID
	sources []types.MessageSource
}

func (r *recordingAddresses) Observe(chat types.JID, source types.MessageSource) {
	r.chats = append(r.chats, chat)
	r.sources = append(r.sources, source)
}

func TestNormalizerReportsTheFormEachChatArrivesIn(t *testing.T) {
	t.Parallel()
	identity := fakeIdentity{lids: map[string]types.JID{peerPN.String(): peerLID}}
	recorder := &recordingAddresses{}
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame, 8), MediaCodec{}, stubNamer{}, identity, nil, recorder)
	source := types.MessageSource{Chat: peerPN, Sender: peerPN}
	if err := normalizer.Handle(context.Background(), &events.Message{
		Info:    types.MessageInfo{MessageSource: source, ID: "wa-1"},
		Message: &waE2E.Message{Conversation: proto.String("hi")},
	}); err != nil {
		t.Fatal(err)
	}
	if len(recorder.chats) != 1 || recorder.chats[0] != peerLID || recorder.sources[0].Chat != peerPN {
		t.Fatalf("observed %v / %v, want the canonical chat with its raw source", recorder.chats, recorder.sources)
	}
}
