package telegramconnector

import (
	"context"
	"fmt"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/tg"
)

type MediaBroker interface {
	Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error)
	Download(context.Context, string) ([]byte, error)
}

type TelegramDownload func(context.Context, tg.InputFileLocationClass) ([]byte, error)

type MediaCodec struct {
	broker   MediaBroker
	download TelegramDownload
}

func NewMediaCodec(broker MediaBroker, download TelegramDownload) MediaCodec {
	return MediaCodec{broker: broker, download: download}
}

func (m MediaCodec) Inbound(
	ctx context.Context,
	media tg.MessageMediaClass,
) ([]*wuphfpb.Attachment, bool, error) {
	metadata, location, ok := inboundMedia(media)
	if !ok {
		return nil, false, nil
	}
	bytes, err := m.download(ctx, location)
	if err != nil {
		return nil, false, fmt.Errorf("download Telegram media: %w", err)
	}
	attachment, err := m.broker.Upload(ctx, metadata, bytes)
	if err != nil {
		return nil, false, fmt.Errorf("store Telegram media in core: %w", err)
	}
	return []*wuphfpb.Attachment{attachment}, true, nil
}

func inboundMedia(
	media tg.MessageMediaClass,
) (*wuphfpb.MediaMetadata, tg.InputFileLocationClass, bool) {
	switch value := media.(type) {
	case *tg.MessageMediaPhoto:
		photoClass, ok := value.GetPhoto()
		if !ok {
			return nil, nil, false
		}
		photo, ok := photoClass.AsNotEmpty()
		if !ok {
			return nil, nil, false
		}
		thumbSize := largestPhotoSize(photo.Sizes)
		if thumbSize == "" {
			return nil, nil, false
		}
		fileName := "telegram-photo.jpg"
		mimeType := "image/jpeg"
		return &wuphfpb.MediaMetadata{
				Kind:     wuphfpb.Attachment_KIND_IMAGE,
				FileName: &fileName,
				MimeType: &mimeType,
			}, &tg.InputPhotoFileLocation{
				ID:            photo.ID,
				AccessHash:    photo.AccessHash,
				FileReference: photo.FileReference,
				ThumbSize:     thumbSize,
			}, true
	case *tg.MessageMediaDocument:
		documentClass, ok := value.GetDocument()
		if !ok {
			return nil, nil, false
		}
		document, ok := documentClass.AsNotEmpty()
		if !ok {
			return nil, nil, false
		}
		size := uint64(document.Size)
		metadata := &wuphfpb.MediaMetadata{
			Kind:      documentKind(document),
			MimeType:  stringPointer(document.MimeType),
			SizeBytes: &size,
			Animated:  documentAnimated(document),
		}
		if name := documentFileName(document); name != "" {
			metadata.FileName = &name
		}
		return metadata, document.AsInputDocumentFileLocation(), true
	default:
		return nil, nil, false
	}
}

func largestPhotoSize(sizes []tg.PhotoSizeClass) string {
	var selected string
	var selectedBytes int
	for _, size := range sizes {
		switch value := size.(type) {
		case *tg.PhotoSize:
			if value.Size > selectedBytes {
				selected, selectedBytes = value.Type, value.Size
			}
		case *tg.PhotoSizeProgressive:
			for _, bytes := range value.Sizes {
				if bytes > selectedBytes {
					selected, selectedBytes = value.Type, bytes
				}
			}
		}
	}
	return selected
}

func documentKind(document *tg.Document) wuphfpb.Attachment_Kind {
	for _, attribute := range document.Attributes {
		if _, ok := attribute.(*tg.DocumentAttributeSticker); ok {
			return wuphfpb.Attachment_KIND_STICKER
		}
	}
	for _, attribute := range document.Attributes {
		switch attribute.(type) {
		case *tg.DocumentAttributeVideo:
			return wuphfpb.Attachment_KIND_VIDEO
		case *tg.DocumentAttributeAudio:
			return wuphfpb.Attachment_KIND_AUDIO
		}
	}
	return wuphfpb.Attachment_KIND_FILE
}

// documentAnimated reports whether a document plays: a GIF (animated mp4) or an
// animated sticker. Telegram marks GIFs with DocumentAttributeAnimated, video
// stickers (WebM) with a video attribute alongside the sticker attribute, and
// Lottie stickers with the application/x-tgsticker mime. Each is forwarded as
// the animated hint so the target can render motion.
func documentAnimated(document *tg.Document) bool {
	var sticker, video bool
	for _, attribute := range document.Attributes {
		switch attribute.(type) {
		case *tg.DocumentAttributeAnimated:
			return true
		case *tg.DocumentAttributeSticker:
			sticker = true
		case *tg.DocumentAttributeVideo:
			video = true
		}
	}
	return sticker && (video || document.MimeType == "application/x-tgsticker")
}

func documentFileName(document *tg.Document) string {
	for _, attribute := range document.Attributes {
		if name, ok := attribute.(*tg.DocumentAttributeFilename); ok {
			return name.FileName
		}
	}
	return ""
}

func stringPointer(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}
