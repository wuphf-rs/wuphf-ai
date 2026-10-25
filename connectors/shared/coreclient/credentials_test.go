package coreclient

import (
	"context"
	"io"
	"net"
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"google.golang.org/grpc"
	"google.golang.org/grpc/metadata"
)

// recordingCore captures the authorization header each call presents.
type recordingCore struct {
	wuphfpb.UnimplementedConnectorTransportServer
	wuphfpb.UnimplementedMediaTransportServer
	authorizations chan []string
}

func (r *recordingCore) record(ctx context.Context) {
	md, _ := metadata.FromIncomingContext(ctx)
	r.authorizations <- md.Get("authorization")
}

func (r *recordingCore) OpenSession(stream grpc.BidiStreamingServer[wuphfpb.ClientFrame, wuphfpb.ServerFrame]) error {
	r.record(stream.Context())
	// End the session once registered; the test only needs the header.
	_, err := stream.Recv()
	return err
}

func (r *recordingCore) UploadMedia(stream grpc.ClientStreamingServer[wuphfpb.UploadMediaFrame, wuphfpb.Attachment]) error {
	r.record(stream.Context())
	for {
		if _, err := stream.Recv(); err == io.EOF {
			return stream.SendAndClose(&wuphfpb.Attachment{MediaId: "m1"})
		} else if err != nil {
			return err
		}
	}
}

func startRecordingCore(t *testing.T) (string, *recordingCore) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	core := &recordingCore{authorizations: make(chan []string, 4)}
	server := grpc.NewServer()
	wuphfpb.RegisterConnectorTransportServer(server, core)
	wuphfpb.RegisterMediaTransportServer(server, core)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)
	return listener.Addr().String(), core
}

func expectAuthorization(t *testing.T, core *recordingCore, want string) {
	t.Helper()
	select {
	case got := <-core.authorizations:
		if len(got) != 1 || got[0] != want {
			t.Fatalf("authorization = %q, want %q", got, want)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the core saw no call")
	}
}

func TestSessionPresentsTheConnectorToken(t *testing.T) {
	address, core := startRecordingCore(t)
	client := New(Options{
		CoreAddress: address,
		ConnectorID: "telegram-main",
		Platform:    wuphfpb.Platform_PLATFORM_TELEGRAM,
		Token:       "tg-secret",
	}, zerolog.Nop())
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = client.runSession(ctx, &flakySender{}, make(chan *wuphfpb.ClientFrame), func() {})
	expectAuthorization(t, core, "Bearer tg-secret")
}

func TestMediaPresentsTheToken(t *testing.T) {
	address, core := startRecordingCore(t)
	media, err := DialMedia(address, "media-secret")
	if err != nil {
		t.Fatal(err)
	}
	defer media.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if _, err := media.Upload(ctx, &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_FILE}, []byte("hi")); err != nil {
		t.Fatalf("Upload() error = %v", err)
	}
	expectAuthorization(t, core, "Bearer media-secret")
}

func TestTokenFromEnv(t *testing.T) {
	t.Setenv("WUPHF_TEST_CORE_TOKEN", "secret")
	if token, err := TokenFromEnv("WUPHF_TEST_CORE_TOKEN"); err != nil || token != "secret" {
		t.Fatalf("TokenFromEnv() = %q, %v", token, err)
	}
	t.Setenv("WUPHF_TEST_CORE_TOKEN", "")
	if _, err := TokenFromEnv("WUPHF_TEST_CORE_TOKEN"); err == nil {
		t.Fatal("an empty token must be refused")
	}
}
