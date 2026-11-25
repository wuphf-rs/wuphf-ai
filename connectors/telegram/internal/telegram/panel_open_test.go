package telegramconnector

import (
	"context"
	"testing"

	"github.com/gotd/td/tg"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/protobuf/proto"
)

// The context names the chat as this connector reports chats, and the topic.
func TestPanelContext(t *testing.T) {
	got := panelContext(&tg.InputPeerChannel{ChannelID: 7}, 100)
	if got == nil || got.Chat != "telegram:channel:7" || got.Thread != "100" {
		t.Fatalf("a topic: %+v", got)
	}
	if got := panelContext(&tg.InputPeerUser{UserID: 5}, 0); got == nil || got.Chat != "telegram:user:5" || got.Thread != "" {
		t.Fatalf("a private chat: %+v", got)
	}
	if got := panelContext(&tg.InputPeerSelf{}, 0); got != nil {
		t.Fatalf("self: %+v", got)
	}
}

type recordedOpen struct{ chat, thread, actor string }

type openRecorder struct{ opens []recordedOpen }

func (r *openRecorder) OpenFor(_ context.Context, chat, thread, actor string) error {
	r.opens = append(r.opens, recordedOpen{chat, thread, actor})
	return nil
}

// OpenPanel opens the panel where the core asks, for the actor it names.
func TestOpenPanelOpensThePanel(t *testing.T) {
	recorder := &openRecorder{}
	sender := Sender{peers: NewPeerCache()}.WithPanel(recorder)
	chat, err := sender.OpenPanel(context.Background(), &wuphfpb.OpenPanelDispatch{
		TargetChatId: "telegram:channel:7", TargetThreadId: proto.String("100"), Actor: "704",
	})
	if err != nil || chat != "telegram:channel:7" {
		t.Fatalf("OpenPanel() = %q, %v", chat, err)
	}
	if len(recorder.opens) != 1 || recorder.opens[0] != (recordedOpen{"telegram:channel:7", "100", "704"}) {
		t.Fatalf("opens = %+v", recorder.opens)
	}
}

// Without a panel the dispatch is only acknowledged.
func TestOpenPanelWithoutAPanel(t *testing.T) {
	chat, err := Sender{peers: NewPeerCache()}.OpenPanel(context.Background(), &wuphfpb.OpenPanelDispatch{TargetChatId: "telegram:chat:1"})
	if err != nil || chat != "telegram:chat:1" {
		t.Fatalf("OpenPanel() = %q, %v", chat, err)
	}
}
