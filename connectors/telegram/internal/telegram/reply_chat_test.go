package telegramconnector

import (
	"testing"

	"github.com/gotd/td/tg"
)

// A reply quoting a message of another chat names that chat; one quoting the
// same chat (any topic) names none (plan 083).
func TestReplyChatID(t *testing.T) {
	forum := &tg.PeerChannel{ChannelID: 7}
	header := func(peer tg.PeerClass) *tg.Message {
		reply := &tg.MessageReplyHeader{ReplyToMsgID: 42}
		if peer != nil {
			reply.SetReplyToPeerID(peer)
		}
		message := &tg.Message{PeerID: forum}
		message.SetReplyTo(reply)
		return message
	}
	if got := replyChatID(header(&tg.PeerChannel{ChannelID: 9})); got == nil || *got != "telegram:channel:9" {
		t.Fatalf("another chat: %v", got)
	}
	if got := replyChatID(header(forum)); got != nil {
		t.Fatalf("the same chat named %q", *got)
	}
	if got := replyChatID(header(nil)); got != nil {
		t.Fatalf("no peer named %q", *got)
	}
	if got := replyChatID(&tg.Message{PeerID: forum}); got != nil {
		t.Fatalf("not a reply named %q", *got)
	}
}
