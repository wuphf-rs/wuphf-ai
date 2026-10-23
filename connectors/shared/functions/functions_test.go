package functions

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"sync/atomic"
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"
	"google.golang.org/protobuf/proto"
)

// patience bounds every wait so a broken test fails instead of hanging.
const patience = 5 * time.Second

// fakeCore implements both function services in memory.
type fakeCore struct {
	wuphfpb.UnimplementedFunctionTransportServer
	wuphfpb.UnimplementedProviderTransportServer

	// invoke answers Invoke; invoked receives each request with its metadata.
	invoke  func(*wuphfpb.InvokeRequest) (*wuphfpb.InvokeResponse, error)
	invoked chan invocation

	// reject, when set, decides whether a session is refused after its hello.
	reject   func(attempt int32) error
	attempts atomic.Int32
	sessions chan *fakeSession
}

type invocation struct {
	request  *wuphfpb.InvokeRequest
	metadata metadata.MD
}

// fakeSession is one accepted provider session; the test drives the stream
// and ends it by sending to end.
type fakeSession struct {
	hello    *wuphfpb.ProviderHello
	metadata metadata.MD
	stream   wuphfpb.ProviderTransport_OpenProviderSessionServer
	end      chan error
}

func (f *fakeCore) Invoke(ctx context.Context, request *wuphfpb.InvokeRequest) (*wuphfpb.InvokeResponse, error) {
	md, _ := metadata.FromIncomingContext(ctx)
	f.invoked <- invocation{request: request, metadata: md}
	return f.invoke(request)
}

func (f *fakeCore) OpenProviderSession(stream wuphfpb.ProviderTransport_OpenProviderSessionServer) error {
	attempt := f.attempts.Add(1)
	first, err := stream.Recv()
	if err != nil {
		return err
	}
	if f.reject != nil {
		if err := f.reject(attempt); err != nil {
			return err
		}
	}
	md, _ := metadata.FromIncomingContext(stream.Context())
	session := &fakeSession{hello: first.GetHello(), metadata: md, stream: stream, end: make(chan error, 1)}
	f.sessions <- session
	select {
	case err := <-session.end:
		return err
	case <-stream.Context().Done():
		return nil
	}
}

func newFakeCore() *fakeCore {
	return &fakeCore{invoked: make(chan invocation, 8), sessions: make(chan *fakeSession, 8)}
}

// start serves core on an in-memory listener and returns the dial option that
// reaches it, for the address "passthrough:///bufnet".
func start(t *testing.T, core *fakeCore) grpc.DialOption {
	t.Helper()
	listener := bufconn.Listen(1 << 20)
	server := grpc.NewServer()
	wuphfpb.RegisterFunctionTransportServer(server, core)
	wuphfpb.RegisterProviderTransportServer(server, core)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)
	return grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
		return listener.DialContext(ctx)
	})
}

const address = "passthrough:///bufnet"

func receive[T any](t *testing.T, channel <-chan T) T {
	t.Helper()
	select {
	case value := <-channel:
		return value
	case <-time.After(patience):
		t.Fatal("timed out waiting")
		panic("unreachable")
	}
}

func TestInvokeSendsArgsTokenAndCaller(t *testing.T) {
	core := newFakeCore()
	core.invoke = func(*wuphfpb.InvokeRequest) (*wuphfpb.InvokeResponse, error) {
		return &wuphfpb.InvokeResponse{Result: []byte(`{"y":2}`), MediaIds: []string{"out"}, ProviderId: "p"}, nil
	}
	client, err := dial(address, "secret", "telegram-main", start(t, core))
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()

	var result struct{ Y int }
	mediaIDs, err := client.Invoke(context.Background(), "test.echo", 1, map[string]int{"x": 1}, []string{"in"}, &result)
	if err != nil {
		t.Fatal(err)
	}
	seen := receive(t, core.invoked)
	if string(seen.request.GetArgs()) != `{"x":1}` {
		t.Errorf("args = %s", seen.request.GetArgs())
	}
	if !proto.Equal(seen.request.GetFunction(), &wuphfpb.FunctionRef{Name: "test.echo", Major: 1}) {
		t.Errorf("function = %v", seen.request.GetFunction())
	}
	if got := seen.request.GetMediaIds(); len(got) != 1 || got[0] != "in" {
		t.Errorf("media ids = %v", got)
	}
	if seen.request.GetCaller() != "telegram-main" {
		t.Errorf("caller = %q", seen.request.GetCaller())
	}
	if got := seen.metadata.Get("authorization"); len(got) != 1 || got[0] != "Bearer secret" {
		t.Errorf("authorization = %v", got)
	}
	if result.Y != 2 || len(mediaIDs) != 1 || mediaIDs[0] != "out" {
		t.Errorf("result = %+v, media ids = %v", result, mediaIDs)
	}
}

func TestInvokeOmitsAuthorizationWithoutToken(t *testing.T) {
	core := newFakeCore()
	core.invoke = func(*wuphfpb.InvokeRequest) (*wuphfpb.InvokeResponse, error) {
		return &wuphfpb.InvokeResponse{Result: []byte(`null`)}, nil
	}
	client, err := dial(address, "", "telegram-main", start(t, core))
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	if _, err := client.Invoke(context.Background(), "test.echo", 1, struct{}{}, nil, nil); err != nil {
		t.Fatal(err)
	}
	if got := receive(t, core.invoked).metadata.Get("authorization"); len(got) != 0 {
		t.Errorf("authorization = %v, want none", got)
	}
}

func TestInvokeReturnsStatusCode(t *testing.T) {
	core := newFakeCore()
	core.invoke = func(*wuphfpb.InvokeRequest) (*wuphfpb.InvokeResponse, error) {
		return nil, status.Error(codes.NotFound, "no configured provider may serve test.echo@1")
	}
	client, err := dial(address, "", "telegram-main", start(t, core))
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	_, err = client.Invoke(context.Background(), "test.echo", 1, struct{}{}, nil, nil)
	if status.Code(err) != codes.NotFound {
		t.Fatalf("code = %v (%v), want NotFound", status.Code(err), err)
	}
}

// serve runs Serve in the background against core and returns a function that
// stops it and returns Serve's error.
func serve(t *testing.T, core *fakeCore, functions []Function) func() error {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Serve(ctx, ProviderOptions{
			Address:        address,
			ProviderID:     "mediaconv",
			Token:          "provider-secret",
			Functions:      functions,
			ReconnectDelay: 10 * time.Millisecond,
			dialOptions:    []grpc.DialOption{start(t, core)},
		}, zerolog.Nop())
	}()
	stop := func() error {
		cancel()
		return receive(t, done)
	}
	t.Cleanup(func() { cancel() })
	return stop
}

func echo(_ context.Context, call Call) (Result, error) {
	return Result{Body: call.Args, MediaIDs: []string{"converted"}}, nil
}

func sendCall(t *testing.T, session *fakeSession, call *wuphfpb.ProviderCall) {
	t.Helper()
	if err := session.stream.Send(&wuphfpb.ProviderServerFrame{Body: &wuphfpb.ProviderServerFrame_Call{Call: call}}); err != nil {
		t.Fatal(err)
	}
}

func receiveResult(t *testing.T, session *fakeSession) *wuphfpb.CallResult {
	t.Helper()
	frames := make(chan *wuphfpb.ProviderClientFrame, 1)
	go func() {
		frame, err := session.stream.Recv()
		if err == nil {
			frames <- frame
		}
	}()
	return receive(t, frames).GetResult()
}

func call(id, name string, args string) *wuphfpb.ProviderCall {
	return &wuphfpb.ProviderCall{
		CallId:         id,
		Function:       &wuphfpb.FunctionRef{Name: name, Major: 1},
		Args:           []byte(args),
		DeadlineUnixMs: time.Now().Add(time.Minute).UnixMilli(),
		Caller:         "telegram-main",
	}
}

func TestServeRegistersAndAnswersCalls(t *testing.T) {
	core := newFakeCore()
	stop := serve(t, core, []Function{{
		Name:            "test.echo",
		Major:           1,
		RequestSchema:   `{"type":"object"}`,
		ResponseSchema:  `{"type":"string"}`,
		Idempotent:      true,
		MaxConcurrency:  4,
		DefaultDeadline: 1500 * time.Millisecond,
		Handle:          echo,
	}})
	session := receive(t, core.sessions)

	deadline := uint32(1500)
	want := &wuphfpb.ProviderHello{ProviderId: "mediaconv", Functions: []*wuphfpb.FunctionDeclaration{{
		Function:          &wuphfpb.FunctionRef{Name: "test.echo", Major: 1},
		RequestSchema:     `{"type":"object"}`,
		ResponseSchema:    `{"type":"string"}`,
		Idempotent:        true,
		MaxConcurrency:    4,
		DefaultDeadlineMs: &deadline,
	}}}
	if !proto.Equal(session.hello, want) {
		t.Errorf("hello = %v, want %v", session.hello, want)
	}
	if got := session.metadata.Get("authorization"); len(got) != 1 || got[0] != "Bearer provider-secret" {
		t.Errorf("authorization = %v", got)
	}

	sendCall(t, session, call("c1", "test.echo", `{"a":1}`))
	result := receiveResult(t, session)
	if result.GetCallId() != "c1" || string(result.GetSuccess().GetResult()) != `{"a":1}` {
		t.Errorf("result = %v", result)
	}
	if got := result.GetSuccess().GetMediaIds(); len(got) != 1 || got[0] != "converted" {
		t.Errorf("media ids = %v", got)
	}
	if err := stop(); !errors.Is(err, context.Canceled) {
		t.Errorf("Serve returned %v, want context.Canceled", err)
	}
}

func TestServeMapsHandlerErrors(t *testing.T) {
	core := newFakeCore()
	serve(t, core, []Function{{Name: "test.fail", Major: 1, Handle: func(_ context.Context, call Call) (Result, error) {
		var kind string
		_ = json.Unmarshal(call.Args, &kind)
		switch kind {
		case "invalid":
			return Result{}, InvalidArgument("bad %s", "args")
		case "unsupported":
			return Result{}, Unsupported("cannot")
		case "panic":
			panic("boom")
		default:
			return Result{}, errors.New("plain failure")
		}
	}}})
	session := receive(t, core.sessions)

	for _, test := range []struct {
		name, args string
		want       wuphfpb.CallErrorCode
		message    string
	}{
		{"test.fail", `"invalid"`, wuphfpb.CallErrorCode_CALL_ERROR_CODE_INVALID_ARGUMENT, "bad args"},
		{"test.fail", `"unsupported"`, wuphfpb.CallErrorCode_CALL_ERROR_CODE_UNSUPPORTED, "cannot"},
		{"test.fail", `"plain"`, wuphfpb.CallErrorCode_CALL_ERROR_CODE_FAILED, "plain failure"},
		{"test.fail", `"panic"`, wuphfpb.CallErrorCode_CALL_ERROR_CODE_FAILED, "handler panicked: boom"},
		{"test.undeclared", `null`, wuphfpb.CallErrorCode_CALL_ERROR_CODE_UNSUPPORTED, "provider mediaconv does not serve test.undeclared@1"},
	} {
		sendCall(t, session, call("c-"+test.args, test.name, test.args))
		failure := receiveResult(t, session).GetError()
		if failure.GetCode() != test.want || failure.GetMessage() != test.message {
			t.Errorf("%s %s: got %v %q, want %v %q", test.name, test.args, failure.GetCode(), failure.GetMessage(), test.want, test.message)
		}
	}
}

func TestServeCancelsHandlerOnCancelCall(t *testing.T) {
	core := newFakeCore()
	started := make(chan struct{})
	cancelled := make(chan error, 1)
	serve(t, core, []Function{{Name: "test.slow", Major: 1, Handle: func(ctx context.Context, _ Call) (Result, error) {
		close(started)
		<-ctx.Done()
		cancelled <- ctx.Err()
		return Result{}, ctx.Err()
	}}})
	session := receive(t, core.sessions)
	sendCall(t, session, call("slow", "test.slow", `{}`))
	receive(t, started)
	if err := session.stream.Send(&wuphfpb.ProviderServerFrame{
		Body: &wuphfpb.ProviderServerFrame_Cancel{Cancel: &wuphfpb.CancelCall{CallId: "slow"}},
	}); err != nil {
		t.Fatal(err)
	}
	if err := receive(t, cancelled); !errors.Is(err, context.Canceled) {
		t.Errorf("handler ctx ended with %v, want context.Canceled", err)
	}
}

func TestServeAppliesCallDeadline(t *testing.T) {
	core := newFakeCore()
	deadlines := make(chan time.Time, 1)
	serve(t, core, []Function{{Name: "test.deadline", Major: 1, Handle: func(ctx context.Context, _ Call) (Result, error) {
		deadline, _ := ctx.Deadline()
		deadlines <- deadline
		return Result{Body: nil}, nil
	}}})
	session := receive(t, core.sessions)
	sent := call("d", "test.deadline", `{}`)
	sendCall(t, session, sent)
	if got, want := receive(t, deadlines), time.UnixMilli(sent.GetDeadlineUnixMs()); !got.Equal(want) {
		t.Errorf("deadline = %v, want %v", got, want)
	}
}

func TestServeStopsOnPermanentRejection(t *testing.T) {
	core := newFakeCore()
	core.reject = func(int32) error {
		return status.Error(codes.PermissionDenied, "provider mediaconv may not register test.echo@1")
	}
	done := make(chan error, 1)
	go func() {
		done <- Serve(context.Background(), ProviderOptions{
			Address:        address,
			ProviderID:     "mediaconv",
			Functions:      []Function{{Name: "test.echo", Major: 1, Handle: echo}},
			ReconnectDelay: 10 * time.Millisecond,
			dialOptions:    []grpc.DialOption{start(t, core)},
		}, zerolog.Nop())
	}()
	err := receive(t, done)
	if status.Code(err) != codes.PermissionDenied {
		t.Fatalf("Serve returned %v, want PermissionDenied", err)
	}
	if attempts := core.attempts.Load(); attempts != 1 {
		t.Errorf("attempts = %d, want 1 (no retry)", attempts)
	}
}

func TestServeReconnectsAfterTransientFailure(t *testing.T) {
	core := newFakeCore()
	serve(t, core, []Function{{Name: "test.echo", Major: 1, Handle: echo}})
	first := receive(t, core.sessions)
	first.end <- status.Error(codes.Unavailable, "core restarting")
	second := receive(t, core.sessions)
	if second.hello.GetProviderId() != "mediaconv" {
		t.Errorf("second hello = %v", second.hello)
	}
	if attempts := core.attempts.Load(); attempts != 2 {
		t.Errorf("attempts = %d, want 2", attempts)
	}
}

type fakeDownloader map[string][]byte

func (f fakeDownloader) Download(_ context.Context, mediaID string) ([]byte, error) {
	data, ok := f[mediaID]
	if !ok {
		return nil, errors.New("unknown media " + mediaID)
	}
	return data, nil
}

func converter(t *testing.T, mediaIDs []string) (MediaConverter, *fakeCore) {
	core := newFakeCore()
	core.invoke = func(*wuphfpb.InvokeRequest) (*wuphfpb.InvokeResponse, error) {
		result, _ := json.Marshal(MediaConvertResult{MimeType: "video/webm", FileName: "sticker.webm"})
		return &wuphfpb.InvokeResponse{Result: result, MediaIds: mediaIDs}, nil
	}
	client, err := dial(address, "", "telegram-main", start(t, core))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return MediaConverter{
		Functions: client,
		Media:     fakeDownloader{"converted": []byte("webm bytes")},
		Timeout:   time.Second,
	}, core
}

func TestMediaConverterDownloadsResult(t *testing.T) {
	media, core := converter(t, []string{"converted"})
	data, err := media.Convert(context.Background(), "source", "image/webp", "sticker.webp", "video/webm")
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "webm bytes" {
		t.Errorf("data = %q", data)
	}
	seen := receive(t, core.invoked).request
	if seen.GetFunction().GetName() != MediaConvertName || seen.GetFunction().GetMajor() != MediaConvertMajor {
		t.Errorf("function = %v", seen.GetFunction())
	}
	if got := seen.GetMediaIds(); len(got) != 1 || got[0] != "source" {
		t.Errorf("media ids = %v", got)
	}
	var args MediaConvertArgs
	if err := json.Unmarshal(seen.GetArgs(), &args); err != nil {
		t.Fatal(err)
	}
	want := MediaConvertArgs{SourceMimeType: "image/webp", SourceFileName: "sticker.webp", TargetMimeType: "video/webm"}
	if args != want {
		t.Errorf("args = %+v, want %+v", args, want)
	}
}

func TestMediaConverterRejectsWrongMediaCount(t *testing.T) {
	for _, mediaIDs := range [][]string{nil, {"converted", "converted"}} {
		media, _ := converter(t, mediaIDs)
		if _, err := media.Convert(context.Background(), "source", "image/webp", "", "video/webm"); err == nil {
			t.Errorf("%d media ids: want an error", len(mediaIDs))
		}
	}
}
