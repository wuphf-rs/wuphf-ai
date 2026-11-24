package settings

import (
	"context"
	"errors"
	"testing"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/grpc"
)

// fakeDeclarer records the declarations it is sent, and refuses them with
// err when set.
type fakeDeclarer struct {
	requests []*wuphfpb.DeclarePluginSettingsRequest
	err      error
}

func (f *fakeDeclarer) DeclareSettings(_ context.Context, in *wuphfpb.DeclarePluginSettingsRequest, _ ...grpc.CallOption) (*wuphfpb.DeclarePluginSettingsResponse, error) {
	f.requests = append(f.requests, in)
	if f.err != nil {
		return nil, f.err
	}
	return &wuphfpb.DeclarePluginSettingsResponse{}, nil
}

func TestConnectPluginDeclaresAndFollowsThePluginSection(t *testing.T) {
	declarer := &fakeDeclarer{}
	connection, err := ConnectPlugin(context.Background(), declarer, "127.0.0.1:1", "observer", "misc", base(), zerolog.Nop())
	if err != nil {
		t.Fatalf("ConnectPlugin() error = %v", err)
	}
	defer connection.Close()

	if len(declarer.requests) != 1 {
		t.Fatalf("declarations = %d, want 1", len(declarer.requests))
	}
	request := declarer.requests[0]
	want, err := Declare(base())
	if err != nil {
		t.Fatal(err)
	}
	if request.GetPluginId() != "misc" || request.GetSettings().GetBaseJson() != want.GetBaseJson() {
		t.Fatalf("declared %+v", request)
	}
	if connection.prefix != "plugin_settings.misc" {
		t.Fatalf("follows %q", connection.prefix)
	}
	if connection.Live.Load() != base() {
		t.Fatalf("starts at %+v", connection.Live.Load())
	}
}

func TestConnectPluginFailsWhenTheCoreRefuses(t *testing.T) {
	refused := errors.New("unauthenticated")
	_, err := ConnectPlugin(context.Background(), &fakeDeclarer{err: refused}, "127.0.0.1:1", "observer", "misc", base(), zerolog.Nop())
	if !errors.Is(err, refused) {
		t.Fatalf("ConnectPlugin() error = %v, want the core's refusal", err)
	}
}

func TestWatchSectionFollowsAPluginPrefix(t *testing.T) {
	follower := &fakeFollower{}
	WatchSection(context.Background(), follower, PluginPrefix("misc"), base(), NewLive(base()), nil, nil)
	if len(follower.prefixes) != 1 || follower.prefixes[0] != "plugin_settings.misc" {
		t.Fatalf("watched %v", follower.prefixes)
	}
}
