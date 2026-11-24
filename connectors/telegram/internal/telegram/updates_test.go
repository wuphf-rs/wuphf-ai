package telegramconnector

import (
	"context"
	"testing"

	"github.com/gotd/td/tg"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

func topicMessage(id int, topID, replyID int) *tg.Message {
	header := &tg.MessageReplyHeader{ForumTopic: true}
	if topID != 0 {
		header.SetReplyToTopID(topID)
	}
	if replyID != 0 {
		header.SetReplyToMsgID(replyID)
	}
	message := &tg.Message{
		ID:      id,
		PeerID:  &tg.PeerChannel{ChannelID: 99},
		Message: "hello",
		Date:    7,
	}
	message.SetFromID(&tg.PeerUser{UserID: 88})
	message.SetReplyTo(header)
	return message
}

func TestNormalizeEmitsRawAddressTopicAndLogicalReply(t *testing.T) {
	t.Parallel()

	normalizer := NewNormalizer(NewPeerCache(), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	event, ok, err := normalizer.normalize(context.Background(), tg.Entities{}, topicMessage(42, 12, 21))
	if err != nil {
		t.Fatalf("normalize() error = %v", err)
	}
	if !ok {
		t.Fatal("normalize() ignored topic message")
	}
	if event.GetChatId() != "telegram:channel:99" || event.GetThreadId() != "12" {
		t.Fatalf("normalize() address = %q/%q", event.GetChatId(), event.GetThreadId())
	}
	if event.GetSender() != "telegram:user:88" || event.GetPlatformMessageId() != "42" {
		t.Fatalf("normalize() identity = %q/%q", event.GetSender(), event.GetPlatformMessageId())
	}
	if event.GetReplyToPlatformMessageId() != "21" || event.GetTimestampMs() != 7000 {
		t.Fatalf("normalize() reply/time = %q/%d", event.GetReplyToPlatformMessageId(), event.GetTimestampMs())
	}
}

func TestNormalizeTopicRootAnchorIsNotLogicalReply(t *testing.T) {
	t.Parallel()

	normalizer := NewNormalizer(NewPeerCache(), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	event, ok, err := normalizer.normalize(context.Background(), tg.Entities{}, topicMessage(42, 0, 12))
	if err != nil {
		t.Fatalf("normalize() error = %v", err)
	}
	if !ok {
		t.Fatal("normalize() ignored topic-root anchored message")
	}
	if event.ReplyToPlatformMessageId != nil {
		t.Fatalf("normalize() exposed topic root as logical reply: %q", event.GetReplyToPlatformMessageId())
	}
	if event.GetThreadId() != "12" {
		t.Fatalf("normalize() thread = %q, want 12", event.GetThreadId())
	}
}

func TestNormalizeSetsChatTitleFromEntities(t *testing.T) {
	t.Parallel()

	normalizer := NewNormalizer(NewPeerCache(), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	entities := tg.Entities{Channels: map[int64]*tg.Channel{99: {ID: 99, Title: "Project Chat"}}}
	message := &tg.Message{ID: 1, PeerID: &tg.PeerChannel{ChannelID: 99}, Message: "hi"}
	event, ok, err := normalizer.normalize(context.Background(), entities, message)
	if err != nil || !ok {
		t.Fatalf("normalize() ok=%v err=%v", ok, err)
	}
	if event.GetChatTitle() != "Project Chat" {
		t.Fatalf("normalize() chat title = %q", event.GetChatTitle())
	}
}

func TestNormalizeSkipsUnsupportedMediaAndOutgoing(t *testing.T) {
	t.Parallel()

	normalizer := NewNormalizer(NewPeerCache(), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	media := &tg.Message{ID: 1, PeerID: &tg.PeerChannel{ChannelID: 99}, Message: "caption"}
	media.SetMedia(&tg.MessageMediaEmpty{})
	if _, ok, err := normalizer.normalize(context.Background(), tg.Entities{}, media); err != nil || ok {
		t.Fatal("normalize() accepted unsupported media")
	}
	outgoing := &tg.Message{ID: 2, PeerID: &tg.PeerChannel{ChannelID: 99}, Message: "mine", Out: true}
	if _, ok, err := normalizer.normalize(context.Background(), tg.Entities{}, outgoing); err != nil || ok {
		t.Fatal("normalize() accepted outgoing message")
	}
}

func TestHandleQueuesMessageCreateFrame(t *testing.T) {
	t.Parallel()

	inputs := make(chan *wuphfpb.ClientFrame, 1)
	normalizer := NewNormalizer(NewPeerCache(), inputs, MediaCodec{})
	message := &tg.Message{ID: 42, PeerID: &tg.PeerChannel{ChannelID: 99}, Message: "hello"}
	if err := normalizer.handle(context.Background(), tg.Entities{}, message); err != nil {
		t.Fatalf("handle() error = %v", err)
	}
	frame := <-inputs
	if frame.GetMessageCreate().GetChatId() != "telegram:channel:99" {
		t.Fatalf("handle() frame = %#v", frame)
	}
}

// A user's message names them as owner_ids do, for the core's owner checks.
func TestNormalizeNamesTheSendersAccount(t *testing.T) {
	t.Parallel()
	normalizer := NewNormalizer(NewPeerCache(), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	event, ok, err := normalizer.normalize(context.Background(), tg.Entities{}, topicMessage(42, 12, 21))
	if err != nil || !ok {
		t.Fatalf("normalize() = %v, %v", ok, err)
	}
	if event.Sender != "telegram:user:88" || event.GetSenderAccount() != "88" {
		t.Fatalf("sender = %q, account = %q", event.Sender, event.GetSenderAccount())
	}
}
