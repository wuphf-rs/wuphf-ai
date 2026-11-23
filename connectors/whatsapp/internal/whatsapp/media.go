package whatsappconnector

import (
	"context"
	"fmt"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
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
	if video := message.GetVideoMessage(); video != nil {
		size := video.GetFileLength()
		// A WhatsApp GIF is an MP4 flagged for GIF playback, not a real GIF; the
		// flag is the only thing distinguishing it from a normal video, so it is
		// carried through as the animated hint for the target to render as a GIF.
		return &wuphfpb.MediaMetadata{
			Kind:      wuphfpb.Attachment_KIND_VIDEO,
			MimeType:  optionalString(video.GetMimetype()),
			SizeBytes: &size,
			Animated:  video.GetGifPlayback(),
		}, video, true
	}
	if audio := message.GetAudioMessage(); audio != nil {
		size := audio.GetFileLength()
		return &wuphfpb.MediaMetadata{
			Kind:      wuphfpb.Attachment_KIND_AUDIO,
			MimeType:  optionalString(audio.GetMimetype()),
			SizeBytes: &size,
		}, audio, true
	}
	if sticker := message.GetStickerMessage(); sticker != nil {
		size := sticker.GetFileLength()
		// WhatsApp animated stickers are animated WebP; the mime is image/webp
		// either way, so IsAnimated is the only signal and is forwarded so the
		// target can render motion (e.g. transcode to a WebM sticker).
		return &wuphfpb.MediaMetadata{
			Kind:      wuphfpb.Attachment_KIND_STICKER,
			MimeType:  optionalString(sticker.GetMimetype()),
			SizeBytes: &size,
			Animated:  sticker.GetIsAnimated(),
		}, sticker, true
	}
	return nil, nil, false
}

func optionalString(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}
