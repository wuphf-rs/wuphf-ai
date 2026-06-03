package instagramconnector

import (
	"testing"

	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

func TestNormalizeBuildsTextCreateEvent(t *testing.T) {
	t.Parallel()
	event, ok := normalize(&table.LSInsertMessage{
		Text:          "hello",
		ThreadKey:     42,
		SenderId:      7,
		MessageId:     "mid_1",
		TimestampMs:   1700000000000,
		ReplySourceId: "mid_0",
	})
	if !ok {
		t.Fatal("normalize() ok = false, want true")
	}
	if event.ChatId != "42" || event.Sender != "7" || event.PlatformMessageId != "mid_1" {
		t.Fatalf("chat = %q, sender = %q, id = %q", event.ChatId, event.Sender, event.PlatformMessageId)
	}
	if event.GetText() != "hello" || event.GetReplyToPlatformMessageId() != "mid_0" {
		t.Fatalf("text = %q, reply = %q", event.GetText(), event.GetReplyToPlatformMessageId())
	}
	if event.TimestampMs != 1700000000000 {
		t.Fatalf("timestamp = %d", event.TimestampMs)
	}
}

func TestNormalizeSkipsNonTextRows(t *testing.T) {
	t.Parallel()
	cases := map[string]*table.LSInsertMessage{
		"empty text": {ThreadKey: 1, MessageId: "m", SenderId: 1},
		"unsent":     {Text: "x", ThreadKey: 1, MessageId: "m", SenderId: 1, IsUnsent: true},
		"no id":      {Text: "x", ThreadKey: 1, SenderId: 1},
	}
	for name, message := range cases {
		if _, ok := normalize(message); ok {
			t.Fatalf("%s: normalize() ok = true, want false", name)
		}
	}
}
