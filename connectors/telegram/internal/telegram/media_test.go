package telegramconnector

import (
	"context"
	"testing"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/tg"
)

type stubMediaBroker struct {
	uploaded *wuphfpb.MediaMetadata
	bytes    []byte
}

func (s *stubMediaBroker) Upload(
	_ context.Context,
	metadata *wuphfpb.MediaMetadata,
	bytes []byte,
) (*wuphfpb.Attachment, error) {
	s.uploaded = metadata
	s.bytes = bytes
	return &wuphfpb.Attachment{
		MediaId:   "media-id",
		Kind:      metadata.Kind,
		FileName:  metadata.FileName,
		MimeType:  metadata.MimeType,
		SizeBytes: metadata.SizeBytes,
	}, nil
}

func (*stubMediaBroker) Download(context.Context, string) ([]byte, error) {
	return nil, nil
}

func TestMediaCodecUploadsInboundDocument(t *testing.T) {
	t.Parallel()

	document := &tg.Document{
		MimeType: "application/pdf",
		Size:     4,
		Attributes: []tg.DocumentAttributeClass{
			&tg.DocumentAttributeFilename{FileName: "receipt.pdf"},
		},
	}
	media := &tg.MessageMediaDocument{}
	media.SetDocument(document)
	broker := &stubMediaBroker{}
	codec := NewMediaCodec(
		broker,
		func(_ context.Context, location tg.InputFileLocationClass) ([]byte, error) {
			if _, ok := location.(*tg.InputDocumentFileLocation); !ok {
				t.Fatalf("download location = %T", location)
			}
			return []byte("data"), nil
		},
	)

	attachments, supported, err := codec.Inbound(context.Background(), media)
	if err != nil {
		t.Fatalf("Inbound() error = %v", err)
	}
	if !supported || len(attachments) != 1 || attachments[0].GetMediaId() != "media-id" {
		t.Fatalf("Inbound() attachments = %#v, supported = %v", attachments, supported)
	}
	if broker.uploaded.GetKind() != wuphfpb.Attachment_KIND_FILE ||
		broker.uploaded.GetFileName() != "receipt.pdf" ||
		broker.uploaded.GetMimeType() != "application/pdf" ||
		broker.uploaded.GetSizeBytes() != 4 {
		t.Fatalf("Upload() metadata = %#v", broker.uploaded)
	}
	if string(broker.bytes) != "data" {
		t.Fatalf("Upload() bytes = %q", broker.bytes)
	}
}

func TestLargestPhotoSizeSelectsLargestVariant(t *testing.T) {
	t.Parallel()

	size := largestPhotoSize([]tg.PhotoSizeClass{
		&tg.PhotoSize{Type: "m", Size: 10},
		&tg.PhotoSizeProgressive{Type: "y", Sizes: []int{5, 25}},
	})
	if size != "y" {
		t.Fatalf("largestPhotoSize() = %q", size)
	}
}
