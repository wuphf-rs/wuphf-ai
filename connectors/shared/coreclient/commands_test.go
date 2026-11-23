package coreclient

import (
	"context"
	"errors"
	"net"
	"sort"
	"testing"
	"time"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/proto"
)

// commandCore records the registration, sends its scripted RunCommands once
// the session opens, and collects the CommandResults.
type commandCore struct {
	wuphfpb.UnimplementedConnectorTransportServer
	script        []*wuphfpb.RunCommand
	registrations chan *wuphfpb.Registration
	results       chan *wuphfpb.CommandResult
}

func (c *commandCore) OpenSession(stream grpc.BidiStreamingServer[wuphfpb.ClientFrame, wuphfpb.ServerFrame]) error {
	frame, err := stream.Recv()
	if err != nil {
		return err
	}
	c.registrations <- frame.GetRegistration()
	for _, command := range c.script {
		if err := stream.Send(&wuphfpb.ServerFrame{Body: &wuphfpb.ServerFrame_RunCommand{RunCommand: command}}); err != nil {
			return err
		}
	}
	for {
		frame, err := stream.Recv()
		if err != nil {
			return err
		}
		if result := frame.GetCommandResult(); result != nil {
			c.results <- result
		}
	}
}

// pingCommands answers ping with pong and fails fail.
type pingCommands struct{}

func (pingCommands) Declarations() []*wuphfpb.ConnectorCommand {
	return []*wuphfpb.ConnectorCommand{{Id: "ping", Title: "Do ping"}, {Id: "fail", Title: "Do fail"}}
}

func (pingCommands) Run(_ context.Context, id string) (string, error) {
	if id == "ping" {
		return "pong", nil
	}
	return "", errors.New("down")
}

// runCommandSession runs a session with commands against a core sending
// script, until want results arrived.
func runCommandSession(t *testing.T, commands Commands, script []*wuphfpb.RunCommand, want int) (*wuphfpb.Registration, []*wuphfpb.CommandResult) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	core := &commandCore{
		script:        script,
		registrations: make(chan *wuphfpb.Registration, 1),
		results:       make(chan *wuphfpb.CommandResult, len(script)),
	}
	server := grpc.NewServer()
	wuphfpb.RegisterConnectorTransportServer(server, core)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)

	client := New(Options{
		CoreAddress: listener.Addr().String(),
		ConnectorID: "whatsapp-main",
		Platform:    wuphfpb.Platform_PLATFORM_WHATSAPP,
		Token:       "wa-secret",
		Commands:    commands,
	}, zerolog.Nop())
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	go func() { _ = client.runSession(ctx, &flakySender{}, make(chan *wuphfpb.ClientFrame), func() {}) }()

	var registration *wuphfpb.Registration
	select {
	case registration = <-core.registrations:
	case <-ctx.Done():
		t.Fatal("the core saw no registration")
	}
	var results []*wuphfpb.CommandResult
	for len(results) < want {
		select {
		case result := <-core.results:
			results = append(results, result)
		case <-ctx.Done():
			t.Fatalf("%d of %d command results arrived", len(results), want)
		}
	}
	sort.Slice(results, func(i, j int) bool { return results[i].GetRequestId() < results[j].GetRequestId() })
	return registration, results
}

func runCommandFrame(id, request string) *wuphfpb.RunCommand {
	return &wuphfpb.RunCommand{RequestId: request, CommandId: id}
}

func TestTheRegistrationDeclaresTheCommands(t *testing.T) {
	registration, _ := runCommandSession(t, pingCommands{}, nil, 0)
	if !proto.Equal(&wuphfpb.Registration{Commands: registration.GetCommands()}, &wuphfpb.Registration{Commands: pingCommands{}.Declarations()}) {
		t.Fatalf("commands = %v", registration.GetCommands())
	}
}

func TestEachCommandIsAnswered(t *testing.T) {
	script := []*wuphfpb.RunCommand{runCommandFrame("ping", "r1"), runCommandFrame("fail", "r2"), runCommandFrame("nope", "r3")}
	_, results := runCommandSession(t, pingCommands{}, script, 3)
	want := []*wuphfpb.CommandResult{
		{RequestId: "r1", Ok: true, Message: "pong"},
		{RequestId: "r2", Ok: false, Message: "down"},
		{RequestId: "r3", Ok: false, Message: `unknown command "nope"`},
	}
	for i := range want {
		if !proto.Equal(results[i], want[i]) {
			t.Fatalf("result %d = %v, want %v", i, results[i], want[i])
		}
	}
}

func TestAConnectorWithoutCommandsRefusesThem(t *testing.T) {
	registration, results := runCommandSession(t, nil, []*wuphfpb.RunCommand{runCommandFrame("ping", "r1")}, 1)
	if len(registration.GetCommands()) != 0 {
		t.Fatalf("declared %v", registration.GetCommands())
	}
	want := &wuphfpb.CommandResult{RequestId: "r1", Ok: false, Message: "this connector runs no commands"}
	if !proto.Equal(results[0], want) {
		t.Fatalf("result = %v, want %v", results[0], want)
	}
}
