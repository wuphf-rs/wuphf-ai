package whatsappconnector

import (
	"context"
	"errors"
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	waCommon "go.mau.fi/whatsmeow/proto/waCommon"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

func TestNormalizeGroupTextAndReply(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, nil, nil, nil, nil)
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
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{title: "Family Group"}, nil, nil, nil)
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
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil)
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
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, nil, nil, nil, nil)
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
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, nil, nil)
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
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, nil, nil)
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
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, nil, nil)
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

func TestNormalizeTagsPeerLinkedDeviceMessages(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, nil, nil)
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:   types.NewJID("15550001111", types.DefaultUserServer),
				Sender: types.JID{User: "15550001111", Server: types.DefaultUserServer, Device: 3},
			},
			ID:       "wa-peer-linked",
			PushName: "Alice",
		},
		Message: &waE2E.Message{Conversation: proto.String("from her desktop")},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() ok=%v err=%v", ok, err)
	}
	if event.GetSenderName() != "Alice (linked device)" {
		t.Fatalf("sender name = %q, want peer linked-device tag", event.GetSenderName())
	}
}

// fakeIdentity is a resolver that strips the device (like the real one) and
// promotes configured phone JIDs to their LID, without touching a whatsmeow
// store.
type fakeIdentity struct{ lids map[string]types.JID }

func (f fakeIdentity) Canonical(_ context.Context, jid types.JID) types.JID {
	jid = jid.ToNonAD()
	if lid, ok := f.lids[jid.String()]; ok {
		return lid
	}
	return jid
}

func (f fakeIdentity) Alternate(_ context.Context, jid types.JID) types.JID {
	jid = jid.ToNonAD()
	if lid, ok := f.lids[jid.String()]; ok {
		return lid
	}
	for phone, lid := range f.lids {
		if lid == jid {
			parsed, err := types.ParseJID(phone)
			if err != nil {
				return types.EmptyJID
			}
			return parsed
		}
	}
	return types.EmptyJID
}

func TestHandleCanonicalizesChatAndEmitsMigrationOnce(t *testing.T) {
	t.Parallel()
	phone := types.NewJID("15550001111", types.DefaultUserServer)
	lid := types.NewJID("88887777", types.HiddenUserServer)
	identity := fakeIdentity{lids: map[string]types.JID{phone.String(): lid}}
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	normalizer := NewNormalizer(inputs, MediaCodec{}, stubNamer{}, identity, nil, nil)
	message := &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{Chat: phone, Sender: phone},
			ID:            "wa-1",
			PushName:      "Alice",
		},
		Message: &waE2E.Message{Conversation: proto.String("hi")},
	}
	if err := normalizer.Handle(context.Background(), message); err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	migration := (<-inputs).GetIdentityMigrated()
	if migration.GetOldChatId() != phone.String() || migration.GetNewChatId() != lid.String() {
		t.Fatalf("first frame = %#v, want phone-to-LID migration", migration)
	}
	create := (<-inputs).GetMessageCreate()
	if create.GetChatId() != lid.String() {
		t.Fatalf("chat id = %q, want canonical LID", create.GetChatId())
	}
	if create.GetParticipant() != lid.String() {
		t.Fatalf("participant = %q, want canonical LID", create.GetParticipant())
	}

	// A second message on the same chat must not re-report the migration, or the
	// migration stream would carry one entry per message.
	message.Info.ID = "wa-2"
	if err := normalizer.Handle(context.Background(), message); err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	if next := (<-inputs).GetMessageCreate(); next.GetPlatformMessageId() != "wa-2" {
		t.Fatalf("second message did not lead with its create; migration re-emitted")
	}
}

func TestNormalizeStripsSenderDeviceFromIdentity(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, nil, nil)
	event, ok, err := normalizer.normalize(context.Background(), &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:   types.NewJID("15550001111", types.DefaultUserServer),
				Sender: types.JID{User: "15550001111", Server: types.DefaultUserServer, Device: 7},
			},
			ID: "wa-dev",
		},
		Message: &waE2E.Message{Conversation: proto.String("hi")},
	})
	if err != nil || !ok {
		t.Fatalf("normalize() ok=%v err=%v", ok, err)
	}
	if event.GetParticipant() != "15550001111@s.whatsapp.net" {
		t.Fatalf("participant = %q, want per-device suffix stripped", event.GetParticipant())
	}
}

func TestNormalizeReactionEmitsToggle(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, nil, nil)
	message := &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:   types.NewJID("15550001111", types.DefaultUserServer),
				Sender: types.NewJID("15550001111", types.DefaultUserServer),
			},
			ID:       "wa-react",
			PushName: "Alice",
		},
		Message: &waE2E.Message{ReactionMessage: &waE2E.ReactionMessage{
			Key:  &waCommon.MessageKey{ID: proto.String("wa-target")},
			Text: proto.String("❤️"),
		}},
	}
	chat := normalizer.canonical(context.Background(), message.Info.Chat)
	event, ok := normalizer.normalizeReaction(context.Background(), message, chat)
	if !ok {
		t.Fatal("normalizeReaction() did not recognise a reaction message")
	}
	if event.GetPlatformMessageId() != "wa-target" {
		t.Fatalf("reacted message = %q, want wa-target", event.GetPlatformMessageId())
	}
	if event.GetEmoji() != "❤️" {
		t.Fatalf("emoji = %q, want ❤️", event.GetEmoji())
	}
	if event.GetSenderName() != "Alice" {
		t.Fatalf("reactor name = %q, want Alice", event.GetSenderName())
	}
}

func TestNormalizeReactionClearedHasEmptyEmoji(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, nil, nil)
	message := &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:     types.NewJID("15550001111", types.DefaultUserServer),
				Sender:   types.NewJID("15550001111", types.DefaultUserServer),
				IsFromMe: true,
			},
			ID: "wa-unreact",
		},
		Message: &waE2E.Message{ReactionMessage: &waE2E.ReactionMessage{
			Key:  &waCommon.MessageKey{ID: proto.String("wa-target")},
			Text: proto.String(""),
		}},
	}
	chat := normalizer.canonical(context.Background(), message.Info.Chat)
	event, ok := normalizer.normalizeReaction(context.Background(), message, chat)
	if !ok || event.GetEmoji() != "" || !event.GetFromMe() {
		t.Fatalf("normalizeReaction() = %#v, %v; want own cleared reaction", event, ok)
	}
}

func TestHandleReceiptEmitsReceiptForDM(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		typ  types.ReceiptType
		want wuphfpb.ReceiptKind
	}{
		{"delivered", types.ReceiptTypeDelivered, wuphfpb.ReceiptKind_RECEIPT_KIND_DELIVERED},
		{"read", types.ReceiptTypeRead, wuphfpb.ReceiptKind_RECEIPT_KIND_READ},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			inputs := make(chan *wuphfpb.ClientFrame, 4)
			normalizer := NewNormalizer(inputs, MediaCodec{}, stubNamer{}, nil, nil, nil)
			err := normalizer.HandleReceipt(context.Background(), &events.Receipt{
				MessageSource: types.MessageSource{
					Chat:   types.NewJID("15550001111", types.DefaultUserServer),
					Sender: types.NewJID("15550001111", types.DefaultUserServer),
				},
				MessageIDs: []types.MessageID{"wa-1"},
				Type:       tc.typ,
			})
			if err != nil {
				t.Fatalf("HandleReceipt() error = %v", err)
			}
			receipt := (<-inputs).GetReceipt()
			if receipt.GetPlatformMessageId() != "wa-1" || receipt.GetChatId() != "15550001111@s.whatsapp.net" {
				t.Fatalf("receipt = %#v", receipt)
			}
			if receipt.GetKind() != tc.want {
				t.Fatalf("receipt kind = %v, want %v", receipt.GetKind(), tc.want)
			}
		})
	}
}

func TestHandleReceiptSkipsGroupAndUnsurfaced(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	normalizer := NewNormalizer(inputs, MediaCodec{}, stubNamer{}, nil, nil, nil)
	// A group receipt is per-participant noise, not a DM signal.
	if err := normalizer.HandleReceipt(context.Background(), &events.Receipt{
		MessageSource: types.MessageSource{Chat: types.NewJID("1", types.GroupServer), IsGroup: true},
		MessageIDs:    []types.MessageID{"x"},
		Type:          types.ReceiptTypeRead,
	}); err != nil {
		t.Fatalf("HandleReceipt() error = %v", err)
	}
	// A self-read carries no peer acknowledgement to surface on the mirror.
	if err := normalizer.HandleReceipt(context.Background(), &events.Receipt{
		MessageSource: types.MessageSource{Chat: types.NewJID("2", types.DefaultUserServer)},
		MessageIDs:    []types.MessageID{"y"},
		Type:          types.ReceiptTypeReadSelf,
	}); err != nil {
		t.Fatalf("HandleReceipt() error = %v", err)
	}
	if len(inputs) != 0 {
		t.Fatalf("emitted %d frames, want none", len(inputs))
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

func TestUnwrapViewOnceExposesInnerMedia(t *testing.T) {
	t.Parallel()
	inner := &waE2E.Message{ImageMessage: &waE2E.ImageMessage{
		Mimetype: proto.String("image/jpeg"),
		ViewOnce: proto.Bool(true),
	}}
	cases := []struct {
		name    string
		wrapped *waE2E.Message
	}{
		{"v1", &waE2E.Message{ViewOnceMessage: &waE2E.FutureProofMessage{Message: inner}}},
		{"v2", &waE2E.Message{ViewOnceMessageV2: &waE2E.FutureProofMessage{Message: inner}}},
		{"v2ext", &waE2E.Message{ViewOnceMessageV2Extension: &waE2E.FutureProofMessage{Message: inner}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := unwrapViewOnce(tc.wrapped)
			if got.GetImageMessage() == nil {
				t.Fatalf("unwrapViewOnce(%s) did not expose the inner image", tc.name)
			}
		})
	}
}

func TestUnwrapViewOncePassesThroughPlainMessage(t *testing.T) {
	t.Parallel()
	plain := &waE2E.Message{Conversation: proto.String("hi")}
	if got := unwrapViewOnce(plain); got != plain {
		t.Fatal("unwrapViewOnce changed a non-view-once message")
	}
	if unwrapViewOnce(nil) != nil {
		t.Fatal("unwrapViewOnce(nil) should return nil")
	}
}

func TestMarkViewOncePrefixesCaption(t *testing.T) {
	t.Parallel()
	img := func(caption string) *waE2E.Message {
		return &waE2E.Message{ImageMessage: &waE2E.ImageMessage{
			ViewOnce: proto.Bool(true),
			Caption:  proto.String(caption),
		}}
	}
	if got := markViewOnce(img(""), ""); got != viewOnceMarker {
		t.Fatalf("captionless view-once = %q, want %q", got, viewOnceMarker)
	}
	if got := markViewOnce(img("hi"), "hi"); got != viewOnceMarker+"\nhi" {
		t.Fatalf("captioned view-once = %q", got)
	}
	plain := &waE2E.Message{ImageMessage: &waE2E.ImageMessage{Caption: proto.String("hi")}}
	if got := markViewOnce(plain, "hi"); got != "hi" {
		t.Fatalf("non-view-once caption changed: %q", got)
	}
}

// A chat WhatsApp already addresses by LID reports no migration of its own, but
// the core still has to learn the phone number behind it: that is the form a
// user writes in a routing rule's chat list.
func TestHandleReportsThePhoneFormOfALIDAddressedChat(t *testing.T) {
	t.Parallel()
	phone := types.NewJID("15550001111", types.DefaultUserServer)
	lid := types.NewJID("88887777", types.HiddenUserServer)
	identity := fakeIdentity{lids: map[string]types.JID{phone.String(): lid}}
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	normalizer := NewNormalizer(inputs, MediaCodec{}, stubNamer{}, identity, nil, nil)
	message := &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:           lid,
				Sender:         lid,
				AddressingMode: types.AddressingModeLID,
			},
			ID:       "wa-1",
			PushName: "Alice",
		},
		Message: &waE2E.Message{Conversation: proto.String("hi")},
	}
	if err := normalizer.Handle(context.Background(), message); err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	migration := (<-inputs).GetIdentityMigrated()
	if migration.GetOldChatId() != phone.String() || migration.GetNewChatId() != lid.String() {
		t.Fatalf("first frame = %#v, want the chat's phone form linked to its LID", migration)
	}
	if create := (<-inputs).GetMessageCreate(); create.GetChatId() != lid.String() {
		t.Fatalf("chat id = %q, want canonical LID", create.GetChatId())
	}

	// The link is reported once per session, not on every message.
	message.Info.ID = "wa-2"
	if err := normalizer.Handle(context.Background(), message); err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	if (<-inputs).GetMessageCreate() == nil {
		t.Fatal("second message re-reported the address link")
	}
}

// fakeDecryptor stands in for the whatsmeow client's message-secret decryption,
// returning a fixed reaction or an error without a live session.
type fakeDecryptor struct {
	reaction *waE2E.ReactionMessage
	err      error
}

func (f fakeDecryptor) DecryptReaction(context.Context, *events.Message) (*waE2E.ReactionMessage, error) {
	return f.reaction, f.err
}

func encryptedReaction() *events.Message {
	return &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:    types.NewJID("123456789", types.GroupServer),
				Sender:  types.NewJID("9988", types.DefaultUserServer),
				IsGroup: true,
			},
			ID:       "wa-enc-react",
			PushName: "Alice",
		},
		Message: &waE2E.Message{EncReactionMessage: &waE2E.EncReactionMessage{
			TargetMessageKey: &waCommon.MessageKey{ID: proto.String("wa-target")},
		}},
	}
}

func TestNormalizeReactionDecryptsGroupReaction(t *testing.T) {
	t.Parallel()
	decryptor := fakeDecryptor{reaction: &waE2E.ReactionMessage{
		Key:  &waCommon.MessageKey{ID: proto.String("wa-target")},
		Text: proto.String("🔥"),
	}}
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, decryptor, nil)
	message := encryptedReaction()
	chat := normalizer.canonical(context.Background(), message.Info.Chat)
	event, ok := normalizer.normalizeReaction(context.Background(), message, chat)
	if !ok {
		t.Fatal("normalizeReaction() did not decrypt an encrypted group reaction")
	}
	if event.GetPlatformMessageId() != "wa-target" || event.GetEmoji() != "🔥" {
		t.Fatalf("normalizeReaction() = %#v, want the decrypted target and emoji", event)
	}
}

func TestNormalizeReactionSkipsUndecryptableReaction(t *testing.T) {
	t.Parallel()
	cases := map[string]reactionDecryptor{
		"no decryptor wired": nil,
		"decryption failed":  fakeDecryptor{err: errors.New("no message secret")},
	}
	for name, decryptor := range cases {
		t.Run(name, func(t *testing.T) {
			normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, stubNamer{}, nil, decryptor, nil)
			message := encryptedReaction()
			chat := normalizer.canonical(context.Background(), message.Info.Chat)
			if event, ok := normalizer.normalizeReaction(context.Background(), message, chat); ok {
				t.Fatalf("normalizeReaction() = %#v, want no reaction", event)
			}
		})
	}
}
