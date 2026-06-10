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
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, nil)
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
	if event.GetChatId() != "123456789@g.us" || event.GetText() != "hello" ||
		event.GetReplyToPlatformMessageId() != "wa-parent" ||
		event.GetParticipant() != "9988@s.whatsapp.net" || event.GetTimestampMs() != 7 {
		t.Fatalf("normalize() event = %#v", event)
	}
}

type stubNamer struct{ title string }

func (s stubNamer) ChatTitle(context.Context, types.JID) string { return s.title }

func (s stubNamer) PersonName(_ context.Context, jid types.JID, pushHint string) string {
	if pushHint != "" {
		return pushHint
	}
	return jid.User
}

func TestNormalizeSetsChatTitleFromNamer(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{title: "Family Group"})
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:    types.NewJID("123456789", types.GroupServer),
				Sender:  types.NewJID("9988", types.DefaultUserServer),
				IsGroup: true,
			},
			ID: "wa-1",
		},
		Message: &waE2E.Message{Conversation: proto.String("hi")},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() ok=%v err=%v", ok, err)
	}
	if event.GetChatTitle() != "Family Group" {
		t.Fatalf("normalize() chat title = %q", event.GetChatTitle())
	}
}

func TestHandleQueuesMessageCreateFrame(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 1)
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil)
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
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, nil)
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

func TestNormalizeDirectChatCarriesParticipant(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{})
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:   types.NewJID("15550001111", types.DefaultUserServer),
				Sender: types.NewJID("15550001111", types.DefaultUserServer),
			},
			ID:       "wa-dm",
			PushName: "Alice",
		},
		Message: &waE2E.Message{Conversation: proto.String("hi")},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() ok=%v err=%v", ok, err)
	}
	if event.GetParticipant() != "15550001111@s.whatsapp.net" {
		t.Fatalf("participant = %q, want sender on a direct chat", event.GetParticipant())
	}
	if event.GetSenderName() != "Alice" {
		t.Fatalf("sender name = %q, want Alice", event.GetSenderName())
	}
}

func TestNormalizeLabelsOwnMessagesAsMe(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{})
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:     types.NewJID("15550001111", types.DefaultUserServer),
				Sender:   types.NewJID("19998887777", types.DefaultUserServer),
				IsFromMe: true,
			},
			ID: "wa-own",
		},
		Message: &waE2E.Message{Conversation: proto.String("mine")},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() ok=%v err=%v", ok, err)
	}
	if event.GetSenderName() != "Me" {
		t.Fatalf("sender name = %q, want Me", event.GetSenderName())
	}
}

func TestNormalizeTagsOwnLinkedDeviceMessages(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{})
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:     types.NewJID("15550001111", types.DefaultUserServer),
				Sender:   types.JID{User: "19998887777", Server: types.DefaultUserServer, Device: 12},
				IsFromMe: true,
			},
			ID: "wa-linked",
		},
		Message: &waE2E.Message{Conversation: proto.String("from desktop")},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() ok=%v err=%v", ok, err)
	}
	if event.GetSenderName() != "Me (linked device)" {
		t.Fatalf("sender name = %q, want linked-device tag", event.GetSenderName())
	}
}

func TestIdentityMigrationTreatsLIDAsCanonical(t *testing.T) {
	t.Parallel()
	phone := types.NewJID("15550001111", types.DefaultUserServer)
	lid := types.NewJID("88887777", types.HiddenUserServer)
	migration := identityMigration(&events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:           phone,
				RecipientAlt:   lid,
				AddressingMode: types.AddressingModePN,
			},
		},
	})
	if migration == nil {
		t.Fatal("identityMigration() = nil, want phone-to-LID migration")
	}
	if migration.GetOldChatId() != phone.String() || migration.GetNewChatId() != lid.String() {
		t.Fatalf("identityMigration() = %#v", migration)
	}
}

func TestIdentityMigrationIgnoresGroupAndAbsentAlt(t *testing.T) {
	t.Parallel()
	if identityMigration(&events.Message{Info: types.MessageInfo{
		MessageSource: types.MessageSource{IsGroup: true},
	}}) != nil {
		t.Fatal("identityMigration() reported migration for a group message")
	}
	if identityMigration(&events.Message{Info: types.MessageInfo{
		MessageSource: types.MessageSource{Chat: types.NewJID("1", types.DefaultUserServer)},
	}}) != nil {
		t.Fatal("identityMigration() reported migration without a recipient alt")
	}
}
