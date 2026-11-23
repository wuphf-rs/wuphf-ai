package telegramconnector

import (
	"context"
	"testing"

	"github.com/gotd/td/bin"
	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/tg"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/telegram/internal/config"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// A changed edit mode applies to the next edit: the sender reads its runtime
// settings on every dispatch.
func TestEditModeChangeAppliesToNextDispatch(t *testing.T) {
	invoker := &recordingInvoker{}
	api := tg.NewClient(invoker)
	live := settings.NewLive(config.Runtime{}.WithDefaults())
	sender := Sender{api: api, sender: message.NewSender(api), peers: NewPeerCache(), runtime: live}
	edit := &wuphfpb.MessageEditDispatch{
		TargetChatId:            "telegram:chat:5",
		TargetPlatformMessageId: "7",
	}
	text := "revised"
	edit.Text = &text

	if _, err := sender.EditMessage(context.Background(), edit); err != nil {
		t.Fatalf("in-place EditMessage() error = %v", err)
	}
	if _, ok := last(invoker.requests).(*tg.MessagesEditMessageRequest); !ok {
		t.Fatalf("in place: last request = %T, want an edit", last(invoker.requests))
	}

	next := live.Load()
	next.Rendering.EditMode = config.EditModeRepost
	live.Set(next)
	if _, err := sender.EditMessage(context.Background(), edit); err != nil {
		t.Fatalf("repost EditMessage() error = %v", err)
	}
	reply, ok := last(invoker.requests).(*tg.MessagesSendMessageRequest)
	if !ok {
		t.Fatalf("repost: last request = %T, want a new message", last(invoker.requests))
	}
	if to, ok := reply.ReplyTo.(*tg.InputReplyToMessage); !ok || to.ReplyToMsgID != 7 {
		t.Fatalf("repost reply-to = %#v, want message 7", reply.ReplyTo)
	}
}

func last(requests []bin.Encoder) bin.Encoder {
	if len(requests) == 0 {
		return nil
	}
	return requests[len(requests)-1]
}
