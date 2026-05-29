package whatsappconnector

import (
	"context"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"google.golang.org/protobuf/proto"
)

type stubDownloader struct{}

func (stubDownloader) Download(context.Context, whatsmeow.DownloadableMessage) ([]byte, error) {
	return []byte("data"), nil
}

type uploadBroker struct {
	metadata *wuphfpb.MediaMetadata
	bytes    []byte
}

func (s *uploadBroker) Upload(_ context.Context, metadata *wuphfpb.MediaMetadata, bytes []byte) (*wuphfpb.Attachment, error) {
	s.metadata, s.bytes = metadata, bytes
	return &wuphfpb.Attachment{MediaId: "media-id", Kind: metadata.Kind}, nil
}

func (*uploadBroker) Download(context.Context, string) ([]byte, error) {
	return nil, nil
}

func TestMediaCodecUploadsInboundImage(t *testing.T) {
	t.Parallel()
	broker := &uploadBroker{}
	codec := NewMediaCodec(broker, stubDownloader{})
	attachments, supported, err := codec.Inbound(context.Background(), &waE2E.Message{
		ImageMessage: &waE2E.ImageMessage{
			Mimetype:   proto.String("image/jpeg"),
			FileLength: proto.Uint64(4),
		},
	})
	if err != nil {
		t.Fatalf("Inbound() error = %v", err)
	}
	if !supported || len(attachments) != 1 || attachments[0].GetMediaId() != "media-id" {
		t.Fatalf("Inbound() = %#v, %v", attachments, supported)
	}
	if broker.metadata.GetKind() != wuphfpb.Attachment_KIND_IMAGE || broker.metadata.GetMimeType() != "image/jpeg" || string(broker.bytes) != "data" {
		t.Fatalf("upload = %#v, %q", broker.metadata, broker.bytes)
	}
}
