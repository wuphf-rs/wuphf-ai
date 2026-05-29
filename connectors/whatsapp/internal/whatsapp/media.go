package whatsappconnector

import (
	"context"
	"fmt"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
)

type MediaBroker interface {
	Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error)
	Download(context.Context, string) ([]byte, error)
}

type Downloader interface {
	Download(context.Context, whatsmeow.DownloadableMessage) ([]byte, error)
}

type MediaCodec struct {
	broker     MediaBroker
	downloader Downloader
}

func NewMediaCodec(broker MediaBroker, downloader Downloader) MediaCodec {
	return MediaCodec{broker: broker, downloader: downloader}
}

func (m MediaCodec) Inbound(ctx context.Context, message *waE2E.Message) ([]*wuphfpb.Attachment, bool, error) {
	metadata, downloadable, ok := inboundMedia(message)
	if !ok {
		return nil, false, nil
	}
	bytes, err := m.downloader.Download(ctx, downloadable)
	if err != nil {
		return nil, false, fmt.Errorf("download WhatsApp media: %w", err)
	}
	attachment, err := m.broker.Upload(ctx, metadata, bytes)
	if err != nil {
		return nil, false, fmt.Errorf("store WhatsApp media in core: %w", err)
	}
	return []*wuphfpb.Attachment{attachment}, true, nil
}

func inboundMedia(message *waE2E.Message) (*wuphfpb.MediaMetadata, whatsmeow.DownloadableMessage, bool) {
	if image := message.GetImageMessage(); image != nil {
		size := image.GetFileLength()
		return &wuphfpb.MediaMetadata{
			Kind:      wuphfpb.Attachment_KIND_IMAGE,
			MimeType:  optionalString(image.GetMimetype()),
			SizeBytes: &size,
		}, image, true
	}
	if document := message.GetDocumentMessage(); document != nil {
		size := document.GetFileLength()
		return &wuphfpb.MediaMetadata{
			Kind:      wuphfpb.Attachment_KIND_FILE,
			FileName:  optionalString(document.GetFileName()),
			MimeType:  optionalString(document.GetMimetype()),
			SizeBytes: &size,
		}, document, true
	}
	return nil, nil, false
}

func optionalString(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}
