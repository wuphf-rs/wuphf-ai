package whatsappconnector

import (
	"context"
	"testing"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

func broadcastMessage(fromMe bool, recipients ...types.BroadcastRecipient) *events.Message {
	return &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:                types.NewJID("1700000000", types.BroadcastServer),
				Sender:              types.NewJID("15550001111", types.DefaultUserServer),
				IsFromMe:            fromMe,
				IsGroup:             true,
				BroadcastRecipients: recipients,
			},
			ID:       "wa-b",
			PushName: "Bob",
		},
		Message: &waE2E.Message{Conversation: proto.String("hello all")},
	}
}

func creates(t *testing.T, inputs chan *wuphfpb.ClientFrame, count int) []*wuphfpb.MessageCreateEvent {
	t.Helper()
	var found []*wuphfpb.MessageCreateEvent
	for len(found) < count {
		frame := <-inputs
		if create := frame.GetMessageCreate(); create != nil {
			found = append(found, create)
		}
	}
	return found
}

// A broadcast someone sent the owner shows in their DM, tagged (plan 085).
func TestAnIncomingBroadcastGoesToTheSendersDM(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil)
	if err := normalizer.Handle(context.Background(), broadcastMessage(false)); err != nil {
		t.Fatal(err)
	}
	create := creates(t, inputs, 1)[0]
	if create.GetChatId() != "15550001111@s.whatsapp.net" || create.GetSenderName() != "Bob [broadcast]" {
		t.Fatalf("create = %v", create)
	}
}

// A broadcast the owner sent from the phone shows in each recipient's DM.
func TestAnOwnBroadcastGoesToEachRecipientsDM(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil)
	message := broadcastMessage(true,
		types.BroadcastRecipient{LID: types.NewJID("111", types.HiddenUserServer)},
		types.BroadcastRecipient{PN: types.NewJID("15550002222", types.DefaultUserServer)},
	)
	if err := normalizer.Handle(context.Background(), message); err != nil {
		t.Fatal(err)
	}
	found := creates(t, inputs, 2)
	if found[0].GetChatId() != "111@lid" || found[1].GetChatId() != "15550002222@s.whatsapp.net" {
		t.Fatalf("chats = %s, %s", found[0].GetChatId(), found[1].GetChatId())
	}
	if found[0].GetSenderName() != "Me [broadcast]" {
		t.Fatalf("sender = %q", found[0].GetSenderName())
	}
}

// Status is not a broadcast list: its stories stay where they are.
func TestStatusIsNotABroadcastList(t *testing.T) {
	t.Parallel()
	message := broadcastMessage(false)
	message.Info.Chat = types.StatusBroadcastJID
	copies, broadcast := broadcastCopies(message)
	if broadcast || len(copies) != 1 || copies[0].Info.Chat != types.StatusBroadcastJID {
		t.Fatalf("copies = %v, %v", copies, broadcast)
	}
}
