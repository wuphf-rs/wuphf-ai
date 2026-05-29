package telegramconnector

import (
	"context"
	"testing"

	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/tg"
)

func testRegistry(t *testing.T, target config.Target) TargetRegistry {
	t.Helper()
	registry, err := NewTargetRegistry(config.File{
		CoreAddress: "127.0.0.1:50051",
		ConnectorID: "telegram-main",
		SessionFile: "./telegram.session.json",
		Endpoints:   []string{target.EndpointID},
		Targets:     []config.Target{target},
	})
	if err != nil {
		t.Fatalf("NewTargetRegistry() error = %v", err)
	}
	return registry
}

func TestNormalizeTopicTextAndLogicalReply(t *testing.T) {
	t.Parallel()

	normalizer := NewNormalizer(testRegistry(t, config.Target{
		EndpointID:         "endpoint-a",
		GroupID:            "group-a",
		Peer:               "example",
		IncomingPeerType:   "channel",
		IncomingPeerID:     99,
		TopicRootMessageID: 12,
	}), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	header := &tg.MessageReplyHeader{}
	header.SetReplyToTopID(12)
	header.SetReplyToMsgID(21)
	message := &tg.Message{
		ID:      42,
		PeerID:  &tg.PeerChannel{ChannelID: 99},
		Message: "hello",
		Date:    7,
	}
	message.SetFromID(&tg.PeerUser{UserID: 88})
	message.SetReplyTo(header)

	event, ok, err := normalizer.normalize(context.Background(), message)
	if err != nil {
		t.Fatalf("normalize() error = %v", err)
	}
	if !ok {
		t.Fatal("normalize() ignored configured topic message")
	}
	if event.GetOriginEndpointId() != "endpoint-a" || event.GetGroupId() != "group-a" {
		t.Fatalf("normalize() target = %q/%q", event.GetOriginEndpointId(), event.GetGroupId())
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

	normalizer := NewNormalizer(testRegistry(t, config.Target{
		EndpointID:         "endpoint-a",
		GroupID:            "group-a",
		Peer:               "example",
		IncomingPeerType:   "channel",
		IncomingPeerID:     99,
		TopicRootMessageID: 12,
	}), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	header := &tg.MessageReplyHeader{}
	header.SetReplyToMsgID(12)
	message := &tg.Message{ID: 42, PeerID: &tg.PeerChannel{ChannelID: 99}, Message: "hello"}
	message.SetReplyTo(header)

	event, ok, err := normalizer.normalize(context.Background(), message)
	if err != nil {
		t.Fatalf("normalize() error = %v", err)
	}
	if !ok {
		t.Fatal("normalize() ignored topic-root anchored message")
	}
	if event.ReplyToPlatformMessageId != nil {
		t.Fatalf("normalize() exposed topic root as logical reply: %q", event.GetReplyToPlatformMessageId())
	}
}

func TestNormalizeSkipsUnsupportedMediaAndUnconfiguredPeer(t *testing.T) {
	t.Parallel()

	normalizer := NewNormalizer(testRegistry(t, config.Target{
		EndpointID:       "endpoint-a",
		GroupID:          "group-a",
		Peer:             "example",
		IncomingPeerType: "channel",
		IncomingPeerID:   99,
	}), make(chan *wuphfpb.ClientFrame), MediaCodec{})
	media := &tg.Message{ID: 1, PeerID: &tg.PeerChannel{ChannelID: 99}, Message: "caption"}
	media.SetMedia(&tg.MessageMediaEmpty{})
	if _, ok, err := normalizer.normalize(context.Background(), media); err != nil || ok {
		t.Fatal("normalize() accepted unsupported media")
	}
	if _, ok, err := normalizer.normalize(context.Background(), &tg.Message{
		ID:      2,
		PeerID:  &tg.PeerChannel{ChannelID: 100},
		Message: "other",
	}); err != nil || ok {
		t.Fatal("normalize() accepted unconfigured peer")
	}
}

func TestHandleQueuesMessageCreateFrame(t *testing.T) {
	t.Parallel()

	inputs := make(chan *wuphfpb.ClientFrame, 1)
	normalizer := NewNormalizer(testRegistry(t, config.Target{
		EndpointID:       "endpoint-a",
		GroupID:          "group-a",
		Peer:             "example",
		IncomingPeerType: "channel",
		IncomingPeerID:   99,
	}), inputs, MediaCodec{})
	if err := normalizer.handle(context.Background(), &tg.Message{
		ID:      42,
		PeerID:  &tg.PeerChannel{ChannelID: 99},
		Message: "hello",
	}); err != nil {
		t.Fatalf("handle() error = %v", err)
	}
	frame := <-inputs
	if frame.GetMessageCreate().GetPlatformMessageId() != "42" {
		t.Fatalf("handle() frame = %#v", frame)
	}
}
