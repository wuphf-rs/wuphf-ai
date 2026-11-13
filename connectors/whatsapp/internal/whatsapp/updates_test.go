package whatsappconnector

import (
	"context"
	"errors"
	"strconv"
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	waCommon "go.mau.fi/whatsmeow/proto/waCommon"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	waHistorySync "go.mau.fi/whatsmeow/proto/waHistorySync"
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
// returning a fixed reaction, opened message or error without a live session.
type fakeDecryptor struct {
	reaction *waE2E.ReactionMessage
	secret   *waE2E.Message
	err      error
}

func (f fakeDecryptor) DecryptReaction(context.Context, *events.Message) (*waE2E.ReactionMessage, error) {
	return f.reaction, f.err
}

func (f fakeDecryptor) DecryptSecretEncryptedMessage(context.Context, *events.Message) (*waE2E.Message, error) {
	return f.secret, f.err
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
	cases := map[string]messageDecryptor{
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

// sealedEdit is an edit made on the account's main device, as WhatsApp
// delivers it: a SecretEncryptedMessage naming the original message.
func sealedEdit(kind waE2E.SecretEncryptedMessage_SecretEncType) *events.Message {
	return &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{
				Chat:     types.NewJID("123456789", types.GroupServer),
				Sender:   types.NewJID("9988", types.DefaultUserServer),
				IsGroup:  true,
				IsFromMe: true,
			},
			ID: "wa-sealed-edit",
		},
		Message: &waE2E.Message{SecretEncryptedMessage: &waE2E.SecretEncryptedMessage{
			TargetMessageKey: &waCommon.MessageKey{ID: proto.String("wa-original")},
			SecretEncType:    kind.Enum(),
		}},
	}
}

func TestHandleBridgesASealedEdit(t *testing.T) {
	t.Parallel()
	for name, opened := range map[string]*waE2E.Message{
		"plain content": {Conversation: proto.String("fixed text")},
		"protocol edit": {ProtocolMessage: &waE2E.ProtocolMessage{
			Type:          waE2E.ProtocolMessage_MESSAGE_EDIT.Enum(),
			EditedMessage: &waE2E.Message{Conversation: proto.String("fixed text")},
		}},
		"edited wrapper": {EditedMessage: &waE2E.FutureProofMessage{
			Message: &waE2E.Message{Conversation: proto.String("fixed text")},
		}},
	} {
		t.Run(name, func(t *testing.T) {
			inputs := make(chan *wuphfpb.ClientFrame, 4)
			normalizer := NewNormalizer(inputs, MediaCodec{}, stubNamer{}, nil, fakeDecryptor{secret: opened}, nil)
			if err := normalizer.Handle(context.Background(), sealedEdit(waE2E.SecretEncryptedMessage_MESSAGE_EDIT)); err != nil {
				t.Fatalf("Handle() error = %v", err)
			}
			// Handle sends synchronously into the buffered channel, so the frame
			// is already there or was never sent.
			var edit *wuphfpb.MessageEditEvent
			select {
			case frame := <-inputs:
				edit = frame.GetMessageEdit()
			default:
			}
			if edit == nil {
				t.Fatal("Handle() did not report the sealed edit")
			}
			if edit.GetPlatformMessageId() != "wa-original" || edit.GetText() != "fixed text" {
				t.Fatalf("edit = %#v, want the original message id and the opened text", edit)
			}
			if edit.GetChatId() != "123456789@g.us" {
				t.Fatalf("edit chat = %q", edit.GetChatId())
			}
		})
	}
}

func TestHandleDropsUnopenableOrNonEditSealedMessages(t *testing.T) {
	t.Parallel()
	cases := map[string]struct {
		decryptor messageDecryptor
		kind      waE2E.SecretEncryptedMessage_SecretEncType
	}{
		"no decryptor wired": {nil, waE2E.SecretEncryptedMessage_MESSAGE_EDIT},
		"decryption failed":  {fakeDecryptor{err: errors.New("no message secret")}, waE2E.SecretEncryptedMessage_MESSAGE_EDIT},
		"poll edit":          {fakeDecryptor{secret: &waE2E.Message{Conversation: proto.String("x")}}, waE2E.SecretEncryptedMessage_POLL_EDIT},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			inputs := make(chan *wuphfpb.ClientFrame, 4)
			normalizer := NewNormalizer(inputs, MediaCodec{}, stubNamer{}, nil, c.decryptor, nil)
			if err := normalizer.Handle(context.Background(), sealedEdit(c.kind)); err != nil {
				t.Fatalf("Handle() error = %v", err)
			}
			select {
			case frame := <-inputs:
				t.Fatalf("Handle() reported %#v, want nothing", frame)
			default:
			}
		})
	}
}

// nextChatUpdate reads frames until a chat update, skipping the identity
// migrations a DM may report first.
func nextChatUpdate(t *testing.T, inputs <-chan *wuphfpb.ClientFrame) *wuphfpb.ChatUpdateEvent {
	t.Helper()
	for {
		select {
		case frame := <-inputs:
			if update := frame.GetChatUpdate(); update != nil {
				return update
			}
		case <-time.After(time.Second):
			t.Fatal("no chat update frame")
		}
	}
}

func timedText(expiration uint32, settingTimestamp int64) *waE2E.Message {
	context := &waE2E.ContextInfo{Expiration: proto.Uint32(expiration)}
	if settingTimestamp != 0 {
		context.EphemeralSettingTimestamp = proto.Int64(settingTimestamp)
	}
	return &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
		Text:        proto.String("hi"),
		ContextInfo: context,
	}}
}

func TestNormalizeCarriesTheMessageTimer(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(make(chan *wuphfpb.ClientFrame), MediaCodec{}, nil, nil, nil, nil)
	normalizeFrom := func(isFromMe bool, message *waE2E.Message) *wuphfpb.MessageCreateEvent {
		event, ok, err := normalizer.normalize(context.Background(), &events.Message{
			Info: types.MessageInfo{
				MessageSource: types.MessageSource{
					Chat:     types.NewJID("1555", types.DefaultUserServer),
					Sender:   types.NewJID("1555", types.DefaultUserServer),
					IsFromMe: isFromMe,
				},
				ID: "wa-1",
			},
			Message: message,
		})
		if err != nil || !ok {
			t.Fatalf("normalize() ok=%v err=%v", ok, err)
		}
		return event
	}

	timer := normalizeFrom(false, timedText(86400, 1700000000)).GetDisappearing()
	if timer.GetSeconds() != 86400 || timer.GetSettingTimestamp() != 1700000000 {
		t.Fatalf("timer = %v, want 86400 set at 1700000000", timer)
	}
	if timer := normalizeFrom(false, timedText(86400, 0)).GetDisappearing(); timer.GetSeconds() != 86400 || timer.SettingTimestamp != nil {
		t.Fatalf("timer = %v, want 86400 with no setting timestamp", timer)
	}
	if timer := normalizeFrom(false, timedText(0, 0)).GetDisappearing(); timer != nil {
		t.Fatalf("an expiration of 0 reported %v", timer)
	}
	if timer := normalizeFrom(false, &waE2E.Message{Conversation: proto.String("hi")}).GetDisappearing(); timer != nil {
		t.Fatalf("a message without a timer reported %v", timer)
	}
	// The owner's own messages carry the chat's timer too.
	if timer := normalizeFrom(true, timedText(604800, 0)).GetDisappearing(); timer.GetSeconds() != 604800 {
		t.Fatalf("own message timer = %v, want 604800", timer)
	}
}

func ephemeralSettingEvent(chat types.JID, seconds uint32, settingTimestamp int64) *events.Message {
	protocol := &waE2E.ProtocolMessage{
		Type:                waE2E.ProtocolMessage_EPHEMERAL_SETTING.Enum(),
		EphemeralExpiration: proto.Uint32(seconds),
	}
	if settingTimestamp != 0 {
		protocol.EphemeralSettingTimestamp = proto.Int64(settingTimestamp)
	}
	return &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{Chat: chat, Sender: chat},
			ID:            "wa-setting",
			Timestamp:     time.Unix(1700000500, 0),
		},
		Message: &waE2E.Message{ProtocolMessage: protocol},
	}
}

func TestHandleReportsEphemeralSetting(t *testing.T) {
	t.Parallel()
	phone := types.NewJID("15550001111", types.DefaultUserServer)
	lid := types.NewJID("88887777", types.HiddenUserServer)
	identity := fakeIdentity{lids: map[string]types.JID{phone.String(): lid}}
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	normalizer := NewNormalizer(inputs, MediaCodec{}, stubNamer{}, identity, nil, nil)

	cases := []struct {
		seconds          uint32
		settingTimestamp int64
		wantTimestamp    int64
	}{
		{604800, 1700000000, 1700000000},
		{0, 1700000100, 1700000100},
		// Without its own timestamp, the setting dates from the message.
		{86400, 0, 1700000500},
	}
	for _, c := range cases {
		if err := normalizer.Handle(context.Background(), ephemeralSettingEvent(phone, c.seconds, c.settingTimestamp)); err != nil {
			t.Fatalf("Handle() error = %v", err)
		}
		updates := nextChatUpdate(t, inputs).GetUpdates()
		if len(updates) != 1 || updates[0].GetChatId() != lid.String() {
			t.Fatalf("updates = %v, want one for the canonical %s", updates, lid)
		}
		timer := updates[0].GetDisappearing()
		if timer == nil || timer.GetSeconds() != c.seconds || timer.GetSettingTimestamp() != c.wantTimestamp {
			t.Fatalf("timer = %v, want %d set at %d", timer, c.seconds, c.wantTimestamp)
		}
	}
	select {
	case frame := <-inputs:
		t.Fatalf("a timer setting also produced %v", frame)
	default:
	}
}

func TestHandleGroupInfoReportsEphemeral(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil, nil, nil, nil)
	group := types.NewJID("123", types.GroupServer)
	at := time.Unix(1700000000, 0)

	for _, c := range []struct {
		ephemeral types.GroupEphemeral
		want      uint32
	}{
		{types.GroupEphemeral{IsEphemeral: true, DisappearingTimer: 604800}, 604800},
		{types.GroupEphemeral{IsEphemeral: false}, 0},
	} {
		ephemeral := c.ephemeral
		if err := normalizer.HandleGroupInfo(context.Background(), &events.GroupInfo{JID: group, Timestamp: at, Ephemeral: &ephemeral}); err != nil {
			t.Fatalf("HandleGroupInfo() error = %v", err)
		}
		updates := nextChatUpdate(t, inputs).GetUpdates()
		if len(updates) != 1 || updates[0].GetChatId() != "123@g.us" ||
			updates[0].GetDisappearing().GetSeconds() != c.want ||
			updates[0].GetDisappearing().GetSettingTimestamp() != 1700000000 {
			t.Fatalf("updates = %v, want %d set at 1700000000", updates, c.want)
		}
	}

	// A notification that changed nothing reported reports nothing.
	if err := normalizer.HandleGroupInfo(context.Background(), &events.GroupInfo{JID: group, Timestamp: at}); err != nil {
		t.Fatalf("HandleGroupInfo() error = %v", err)
	}
	select {
	case frame := <-inputs:
		t.Fatalf("an empty group notification produced %v", frame)
	default:
	}
}

func TestHandleHistorySyncBatches(t *testing.T) {
	t.Parallel()
	phone := types.NewJID("15550001111", types.DefaultUserServer)
	lid := types.NewJID("88887777", types.HiddenUserServer)
	identity := fakeIdentity{lids: map[string]types.JID{phone.String(): lid}}
	inputs := make(chan *wuphfpb.ClientFrame, 8)
	normalizer := NewNormalizer(inputs, MediaCodec{}, nil, identity, nil, nil)

	conversations := []*waHistorySync.Conversation{
		{ID: proto.String(phone.String()), EphemeralExpiration: proto.Uint32(86400), EphemeralSettingTimestamp: proto.Int64(1700000000)},
		{ID: proto.String("status@broadcast"), EphemeralExpiration: proto.Uint32(86400)},
		{ID: proto.String("123@g.us")},
	}
	// Two of the three above are reported; fill up to 1201 reported chats.
	for n := 2; n < 1201; n++ {
		id := types.NewJID(strconv.Itoa(10000+n), types.DefaultUserServer).String()
		conversations = append(conversations, &waHistorySync.Conversation{ID: proto.String(id)})
	}
	if err := normalizer.HandleHistorySync(context.Background(), &events.HistorySync{Data: &waHistorySync.HistorySync{
		SyncType:      waHistorySync.HistorySync_INITIAL_BOOTSTRAP.Enum(),
		Conversations: conversations,
	}}); err != nil {
		t.Fatalf("HandleHistorySync() error = %v", err)
	}
	var sizes []int
	var updates []*wuphfpb.ChatUpdate
	for len(updates) < 1201 {
		event := nextChatUpdate(t, inputs)
		if !event.GetSnapshot() {
			t.Fatal("a history sync frame is not a snapshot")
		}
		batch := event.GetUpdates()
		sizes = append(sizes, len(batch))
		updates = append(updates, batch...)
	}
	if len(sizes) != 3 || sizes[0] != 500 || sizes[1] != 500 || sizes[2] != 201 {
		t.Fatalf("batch sizes = %v, want [500 500 201]", sizes)
	}
	first := updates[0]
	if first.GetChatId() != lid.String() || first.GetDisappearing().GetSeconds() != 86400 ||
		first.GetDisappearing().GetSettingTimestamp() != 1700000000 {
		t.Fatalf("first update = %v, want the canonical LID with 86400 set at 1700000000", first)
	}
	group := updates[1]
	if group.GetChatId() != "123@g.us" || group.GetDisappearing() == nil || group.GetDisappearing().GetSeconds() != 0 ||
		group.GetDisappearing().SettingTimestamp != nil {
		t.Fatalf("group update = %v, want an off timer with no setting timestamp", group)
	}
	for _, update := range updates {
		if update.GetChatId() == "status@broadcast" {
			t.Fatal("the status broadcast was reported")
		}
	}
}
