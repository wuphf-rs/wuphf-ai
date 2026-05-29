package telegramconnector

import (
	"testing"

	"github.com/gotd/td/tg"
)

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
