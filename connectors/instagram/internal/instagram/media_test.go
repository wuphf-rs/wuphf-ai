package instagramconnector

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"strings"
	"testing"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// stubCDN answers every request with one canned response.
type stubCDN struct {
	status int
	body   io.Reader
	urls   []string
}

func (s *stubCDN) Do(request *http.Request) (*http.Response, error) {
	s.urls = append(s.urls, request.URL.String())
	status := s.status
	if status == 0 {
		status = http.StatusOK
	}
	return &http.Response{
		StatusCode: status,
		Status:     http.StatusText(status),
		Header:     http.Header{"Content-Type": []string{"video/mp4"}},
		Body:       io.NopCloser(s.body),
	}, nil
}

// recordingBroker keeps every upload and returns a handle per upload.
type recordingBroker struct {
	uploads [][]byte
	kinds   []wuphfpb.Attachment_Kind
}

func (b *recordingBroker) Upload(_ context.Context, metadata *wuphfpb.MediaMetadata, data []byte) (*wuphfpb.Attachment, error) {
	b.uploads = append(b.uploads, data)
	b.kinds = append(b.kinds, metadata.Kind)
	return &wuphfpb.Attachment{MediaId: fmt.Sprintf("media-%d", len(b.uploads))}, nil
}

func videoRef() mediaRef {
	return mediaRef{metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_VIDEO}, url: "https://cdn.example/v.mp4"}
}

func TestFetchUploadsBodyUnderCap(t *testing.T) {
	t.Parallel()
	broker := &recordingBroker{}
	codec := NewMediaCodec(broker, &stubCDN{body: strings.NewReader("video bytes")}, nil)
	if _, err := codec.fetch(context.Background(), videoRef()); err != nil {
		t.Fatalf("fetch() error = %v", err)
	}
	if len(broker.uploads) != 1 || string(broker.uploads[0]) != "video bytes" {
		t.Fatalf("uploads = %q", broker.uploads)
	}
}

func TestFetchRefusesBodyOverCap(t *testing.T) {
	t.Parallel()
	broker := &recordingBroker{}
	body := bytes.NewReader(make([]byte, maxDownloadBytes+1))
	codec := NewMediaCodec(broker, &stubCDN{body: body}, nil)
	_, err := codec.fetch(context.Background(), videoRef())
	if err == nil || !strings.Contains(err.Error(), "exceeds") {
		t.Fatalf("fetch() error = %v; want an over-cap error", err)
	}
	if len(broker.uploads) != 0 {
		t.Fatalf("uploads = %d; an over-cap body must not reach the core", len(broker.uploads))
	}
}

func TestFetchRefusesNonOKStatus(t *testing.T) {
	t.Parallel()
	broker := &recordingBroker{}
	codec := NewMediaCodec(broker, &stubCDN{status: http.StatusForbidden, body: strings.NewReader("")}, nil)
	_, err := codec.fetch(context.Background(), videoRef())
	if err == nil || !strings.Contains(err.Error(), "unexpected status") {
		t.Fatalf("fetch() error = %v; want an unexpected-status error", err)
	}
	if len(broker.uploads) != 0 {
		t.Fatalf("uploads = %d", len(broker.uploads))
	}
}
