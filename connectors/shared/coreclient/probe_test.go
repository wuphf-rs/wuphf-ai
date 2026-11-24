package coreclient

import (
	"context"
	"testing"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

type fakeProber struct{ asked []string }

func (f *fakeProber) Probe(_ context.Context, chatIDs []string) []*wuphfpb.ChatFacts {
	f.asked = chatIDs
	return []*wuphfpb.ChatFacts{{ChatId: chatIDs[0], Forum: Fact(false)}}
}

func TestAProbeIsAnsweredWithFactsAndTools(t *testing.T) {
	prober := &fakeProber{}
	tools := func() []*wuphfpb.ToolStatus {
		return []*wuphfpb.ToolStatus{{Name: "webpmux", NeededFor: "sticker pack names"}}
	}
	frame := answerProbe(context.Background(), prober, tools, &wuphfpb.ProbeChats{RequestId: "r1", ChatIds: []string{"-100"}})
	result := frame.GetProbeResult()
	if result.GetRequestId() != "r1" || len(prober.asked) != 1 {
		t.Fatalf("result = %v, asked = %v", result, prober.asked)
	}
	if result.GetChats()[0].GetForum() != wuphfpb.Fact_FACT_NO || result.GetTools()[0].GetName() != "webpmux" {
		t.Fatalf("result = %v", result)
	}
	// Without a prober every chat is unknown.
	if got := answerProbe(context.Background(), nil, nil, &wuphfpb.ProbeChats{RequestId: "r2"}).GetProbeResult(); len(got.GetChats()) != 0 || got.GetRequestId() != "r2" {
		t.Fatalf("without a prober: %v", got)
	}
}

func TestToolStatusLooksOnPath(t *testing.T) {
	if !ToolStatus("sh", "").GetAvailable() {
		t.Fatal("sh is on PATH")
	}
	if ToolStatus("wuphf-no-such-tool", "x").GetAvailable() {
		t.Fatal("found a tool that does not exist")
	}
}
