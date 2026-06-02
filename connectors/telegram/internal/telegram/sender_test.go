package telegramconnector

import (
	"testing"

	"github.com/gotd/td/tg"
)

func TestAttributedText(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name        string
		senderName  string
		participant string
		text        string
		wantOptions int
	}{
		{"group message gets bold name plus body", "Alice", "p1", "hi", 2},
		{"group message falls back to participant id", "", "p1", "hi", 2},
		{"group header only when body empty", "Alice", "p1", "", 1},
		{"direct message left unlabelled", "Alice", "", "hi", 1},
		{"nothing to send yields no options", "", "", "", 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := attributedText(tc.senderName, tc.participant, tc.text); len(got) != tc.wantOptions {
				t.Fatalf("attributedText() = %d options, want %d", len(got), tc.wantOptions)
			}
		})
	}
}

func TestSentMessageID(t *testing.T) {
	t.Parallel()

	id, err := sentMessageID(&tg.Updates{
		Updates: []tg.UpdateClass{
			&tg.UpdateNewChannelMessage{Message: &tg.Message{ID: 42}},
		},
	})
	if err != nil {
		t.Fatalf("sentMessageID() error = %v", err)
	}
	if id != 42 {
		t.Fatalf("sentMessageID() = %d, want 42", id)
	}
}
