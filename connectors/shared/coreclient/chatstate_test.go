package coreclient

import (
	"context"
	"net"
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/proto"
)

// chatStateCore sends one ChatState dispatch after the registration and
// records the acknowledgement the connector answers with.
type chatStateCore struct {
	wuphfpb.UnimplementedConnectorTransportServer
	acknowledgements chan *wuphfpb.Acknowledgement
}

func (c *chatStateCore) OpenSession(stream grpc.BidiStreamingServer[wuphfpb.ClientFrame, wuphfpb.ServerFrame]) error {
	if _, err := stream.Recv(); err != nil {
		return err
	}
	if err := stream.Send(&wuphfpb.ServerFrame{Body: &wuphfpb.ServerFrame_ChatState{ChatState: &wuphfpb.ChatStateDispatch{
		DispatchId:     "d-1",
		TargetChatId:   "telegram:channel:1",
		TargetThreadId: proto.String("57"),
		Pinned:         proto.Bool(true),
	}}}); err != nil {
		return err
	}
	for {
		frame, err := stream.Recv()
		if err != nil {
			return err
		}
		if ack := frame.GetAcknowledgement(); ack != nil {
			c.acknowledgements <- ack
		}
	}
}

func startChatStateCore(t *testing.T) (string, *chatStateCore) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	core := &chatStateCore{acknowledgements: make(chan *wuphfpb.Acknowledgement, 1)}
	server := grpc.NewServer()
	wuphfpb.RegisterConnectorTransportServer(server, core)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)
	return listener.Addr().String(), core
}

// stateSender records the chat-state dispatches it applies.
type stateSender struct {
	flakySender
	applied chan *wuphfpb.ChatStateDispatch
}

func (s *stateSender) ApplyChatState(_ context.Context, dispatch *wuphfpb.ChatStateDispatch) (string, error) {
	s.applied <- dispatch
	return dispatch.GetTargetChatId(), nil
}

func runChatStateSession(t *testing.T, sender Sender) *wuphfpb.Acknowledgement {
	t.Helper()
	address, core := startChatStateCore(t)
	client := New(Options{
		CoreAddress: address,
		ConnectorID: "telegram-main",
		Platform:    wuphfpb.Platform_PLATFORM_TELEGRAM,
		Token:       "tg-secret",
	}, zerolog.Nop())
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	go func() { _ = client.runSession(ctx, sender, make(chan *wuphfpb.ClientFrame), func() {}) }()
	select {
	case ack := <-core.acknowledgements:
		return ack
	case <-time.After(5 * time.Second):
		t.Fatal("the chat-state dispatch was not acknowledged")
		return nil
	}
}

func TestAChatStateDispatchIsAcknowledgedWithTheChatIDByDefault(t *testing.T) {
	ack := runChatStateSession(t, &flakySender{})
	if ack.GetDispatchId() != "d-1" || ack.GetPlatformMessageId() != "telegram:channel:1" {
		t.Fatalf("ack = %v, want d-1 acknowledged with the target chat id", ack)
	}
}

func TestAChatStateApplierIsCalled(t *testing.T) {
	sender := &stateSender{applied: make(chan *wuphfpb.ChatStateDispatch, 1)}
	ack := runChatStateSession(t, sender)
	select {
	case dispatch := <-sender.applied:
		if dispatch.GetTargetThreadId() != "57" || !dispatch.GetPinned() {
			t.Fatalf("applied %v, want the topic pinned", dispatch)
		}
	default:
		t.Fatal("the sender's ApplyChatState was not called")
	}
	if ack.GetPlatformMessageId() != "telegram:channel:1" {
		t.Fatalf("ack = %v, want the target chat id", ack)
	}
}
