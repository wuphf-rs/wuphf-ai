package instagramconnector

import (
	"context"
	"strconv"
	"testing"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

func TestAKnownThreadIsWritableAndAnUnknownOneIsGone(t *testing.T) {
	threads, _ := newTestDirectory()
	answers := prober{threads: threads}.Probe(context.Background(), []string{
		strconv.FormatInt(groupKey, 10), "999", "not-a-number",
	})
	if answers[0].GetCanSend() != wuphfpb.Fact_FACT_YES || answers[0].GetForum() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("a listed thread: %v", answers[0])
	}
	if answers[1].GetExists() != wuphfpb.Fact_FACT_NO {
		t.Fatalf("a thread Instagram does not know: %v", answers[1])
	}
	if answers[2].GetExists() != wuphfpb.Fact_FACT_UNKNOWN || answers[2].GetDetail() == "" {
		t.Fatalf("a malformed id: %v", answers[2])
	}
}
