package whatsappconnector

import (
	"bytes"
	"context"
	"testing"

	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
)

// uploadRecordingClient keeps the bytes of every upload.
type uploadRecordingClient struct {
	stubSendClient
	uploads [][]byte
}

func (u *uploadRecordingClient) Upload(ctx context.Context, data []byte, media whatsmeow.MediaType) (whatsmeow.UploadResponse, error) {
	u.uploads = append(u.uploads, append([]byte(nil), data...))
	return u.stubSendClient.Upload(ctx, data, media)
}

// A changed sticker pack name applies to the next sticker: the sender reads
// its runtime settings on every dispatch.
func TestStickerMetadataChangeAppliesToNextSticker(t *testing.T) {
	t.Parallel()
	requireTool(t, "magick")
	requireTool(t, "webpmux")
	client := &uploadRecordingClient{}
	live := settings.NewLive(config.Runtime{Stickers: config.StickerMetadata{PackName: "first pack"}})
	sender := NewSender(client, &stubBroker{bytes: sampleWebP(t)}, stubConverter{}, staticAddressing{}, live)
	mimeType, fileName := "image/webp", "sticker.webp"
	send := func() {
		t.Helper()
		if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
			TargetChatId: "123@s.whatsapp.net",
			Attachments: []*wuphfpb.Attachment{{
				MediaId:  "media-1",
				Kind:     wuphfpb.Attachment_KIND_STICKER,
				MimeType: &mimeType,
				FileName: &fileName,
			}},
		}); err != nil {
			t.Fatalf("SendMessage() error = %v", err)
		}
	}

	send()
	next := live.Load()
	next.Stickers.PackName = "second pack"
	live.Set(next)
	send()

	if len(client.uploads) != 2 {
		t.Fatalf("uploads = %d, want 2", len(client.uploads))
	}
	for index, want := range []string{"first pack", "second pack"} {
		if !bytes.Contains(client.uploads[index], []byte(`"sticker-pack-name":"`+want+`"`)) {
			t.Fatalf("sticker %d does not carry pack name %q", index+1, want)
		}
	}
}
