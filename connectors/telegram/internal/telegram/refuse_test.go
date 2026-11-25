package telegramconnector

import (
	"context"
	"testing"
	"time"

	"github.com/gotd/td/tg"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// A refused message gets 👎 and a reply note that is deleted once its TTL
// has passed (plan 083).
func TestRefuseMessageMarksAndPostsANoteThatGoes(t *testing.T) {
	api := &recordingAPI{}
	sender := NewSender(tg.NewClient(api), NewPeerCache(), nil, nil, nil, false)
	var waited time.Duration
	var expire func()
	sender.after = func(d time.Duration, f func()) { waited, expire = d, f }

	id, err := sender.RefuseMessage(context.Background(), &wuphfpb.MessageRefusedDispatch{
		TargetChatId:            "telegram:chat:42",
		TargetPlatformMessageId: "9",
		Reason:                  "Not sent: Status is receive-only.",
		NoteTtlSecs:             30,
	})
	if err != nil || id != "9" {
		t.Fatalf("RefuseMessage() = %q, %v", id, err)
	}
	reactions := sent[*tg.MessagesSendReactionRequest](api)
	if len(reactions) != 1 || reactions[0].MsgID != 9 {
		t.Fatalf("reactions = %#v, want one on message 9", reactions)
	}
	if emoji, ok := reactions[0].Reaction[0].(*tg.ReactionEmoji); !ok || emoji.Emoticon != "👎" {
		t.Fatalf("reaction = %#v, want 👎", reactions[0].Reaction)
	}
	notes := sent[*tg.MessagesSendMessageRequest](api)
	if len(notes) != 1 || notes[0].Message != "Not sent: Status is receive-only." {
		t.Fatalf("notes = %#v", notes)
	}
	reply, ok := notes[0].ReplyTo.(*tg.InputReplyToMessage)
	if !ok || reply.ReplyToMsgID != 9 {
		t.Fatalf("note replies to %#v, want message 9", notes[0].ReplyTo)
	}
	if waited != 30*time.Second || expire == nil {
		t.Fatalf("note deletion scheduled after %v", waited)
	}
	if deleted := sent[*tg.MessagesDeleteMessagesRequest](api); len(deleted) != 0 {
		t.Fatalf("deleted %#v before the TTL", deleted)
	}
	expire()
	deleted := sent[*tg.MessagesDeleteMessagesRequest](api)
	if len(deleted) != 1 || len(deleted[0].ID) != 1 {
		t.Fatalf("deleted = %#v, want the note", deleted)
	}
}

// A TTL of 0 keeps the note.
func TestRefuseMessageWithoutATTLKeepsTheNote(t *testing.T) {
	api := &recordingAPI{}
	sender := NewSender(tg.NewClient(api), NewPeerCache(), nil, nil, nil, false)
	sender.after = func(time.Duration, func()) { t.Fatal("scheduled a deletion") }
	if _, err := sender.RefuseMessage(context.Background(), &wuphfpb.MessageRefusedDispatch{
		TargetChatId: "telegram:chat:42", TargetPlatformMessageId: "9", Reason: "no",
	}); err != nil {
		t.Fatal(err)
	}
}
