package whatsappconnector

import (
	"context"
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

func TestNormalizeGroupTextAndReply(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(testRegistry(t), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:    types.NewJID("123456789", types.GroupServer),
				Sender:  types.NewJID("9988", types.DefaultUserServer),
				IsGroup: true,
			},
			ID:        "wa-1",
			Timestamp: time.UnixMilli(7),
		},
		Message: &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
			Text: proto.String("hello"),
			ContextInfo: &waE2E.ContextInfo{
				StanzaID: proto.String("wa-parent"),
			},
		}},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() = %#v, %v, %v", event, ok, err)
	}
	if event.GetText() != "hello" || event.GetReplyToPlatformMessageId() != "wa-parent" ||
		event.GetParticipant() != "9988@s.whatsapp.net" || event.GetTimestampMs() != 7 {
		t.Fatalf("normalize() event = %#v", event)
	}
}

func TestHandleQueuesMessageCreateFrame(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 1)
	normalizer := NewNormalizer(testRegistry(t), inputs, MediaCodec{})
	if err := normalizer.Handle(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:   types.NewJID("123456789", types.GroupServer),
				Sender: types.NewJID("1", types.DefaultUserServer),
			},
			ID: "wa-2",
		},
		Message: &waE2E.Message{Conversation: proto.String("hello")},
	}); err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	if (<-inputs).GetMessageCreate().GetPlatformMessageId() != "wa-2" {
		t.Fatal("Handle() did not queue normalized frame")
	}
}

func TestNormalizeIncludesOwnUserMessagesForCoreEchoDecision(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(testRegistry(t), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:     types.NewJID("123456789", types.GroupServer),
				Sender:   types.NewJID("1", types.DefaultUserServer),
				IsFromMe: true,
			},
			ID: "wa-self",
		},
		Message: &waE2E.Message{Conversation: proto.String("from linked device")},
	})
	if err != nil || !ok || event.GetPlatformMessageId() != "wa-self" {
		t.Fatalf("normalize() = %#v, %v, %v", event, ok, err)
	}
}
