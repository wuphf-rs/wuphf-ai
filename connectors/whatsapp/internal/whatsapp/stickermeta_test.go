package whatsappconnector

import (
	"bytes"
	"context"
	"os"
	"os/exec"
	"strings"
	"testing"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
)

// TestStickerExifContainsPackFields checks the EXIF payload carries the
// configured pack metadata and the project link as both store links.
func TestStickerExifContainsPackFields(t *testing.T) {
	t.Parallel()
	exif, err := stickerExif(config.StickerMetadata{
		PackName:   "wuphf",
		AuthorName: "wuphf-ai",
		Link:       "https://github.com/wuphf/wuphf-ai",
	})
	if err != nil {
		t.Fatalf("stickerExif() error = %v", err)
	}
	for _, want := range []string{
		`"sticker-pack-name":"wuphf"`,
		`"sticker-pack-publisher":"wuphf-ai"`,
		`"android-app-store-link":"https://github.com/wuphf/wuphf-ai"`,
		`"ios-app-store-link":"https://github.com/wuphf/wuphf-ai"`,
	} {
		if !bytes.Contains(exif, []byte(want)) {
			t.Fatalf("exif missing %s\ngot: %s", want, exif)
		}
	}
	if !bytes.HasPrefix(exif, []byte{0x49, 0x49, 0x2A, 0x00}) {
		t.Fatalf("exif missing little-endian TIFF header")
	}
}

// TestStickerExifOmitsLinkWhenBlank keeps the store links out when no link is
// configured, so WhatsApp shows no dangling link.
func TestStickerExifOmitsLinkWhenBlank(t *testing.T) {
	t.Parallel()
	exif, err := stickerExif(config.StickerMetadata{PackName: "p", AuthorName: "a"})
	if err != nil {
		t.Fatalf("stickerExif() error = %v", err)
	}
	if bytes.Contains(exif, []byte("app-store-link")) {
		t.Fatalf("exif should omit store links when link is blank: %s", exif)
	}
}

// TestApplyStickerMetadataDegrades returns the original bytes when the sticker
// cannot be rewritten (here, invalid WebP input), so a message still sends.
func TestApplyStickerMetadataDegrades(t *testing.T) {
	t.Parallel()
	original := []byte("not a real webp")
	out := applyStickerMetadata(context.Background(), original, config.StickerMetadata{PackName: "p"})
	if !bytes.Equal(out, original) {
		t.Fatalf("expected original bytes on failure, got %d bytes", len(out))
	}
}

// TestWriteStickerMetadataRoundTrips embeds metadata into a real WebP and reads
// it back with webpmux, confirming WhatsApp would find the pack fields.
func TestWriteStickerMetadataRoundTrips(t *testing.T) {
	t.Parallel()
	requireTool(t, "magick")
	requireTool(t, "webpmux")
	ctx := context.Background()

	out, err := writeStickerMetadata(ctx, sampleWebP(t), config.StickerMetadata{
		PackName:   "wuphf",
		AuthorName: "wuphf-ai",
		Link:       "https://github.com/wuphf/wuphf-ai",
	})
	if err != nil {
		t.Fatalf("writeStickerMetadata() error = %v", err)
	}

	dir := t.TempDir()
	src := dir + "/out.webp"
	gotExif := dir + "/got.exif"
	if err := os.WriteFile(src, out, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := exec.CommandContext(ctx, "webpmux", "-get", "exif", src, "-o", gotExif).Run(); err != nil {
		t.Fatalf("webpmux -get exif: %v", err)
	}
	data, err := os.ReadFile(gotExif)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Contains(data, []byte("https://github.com/wuphf/wuphf-ai")) {
		t.Fatalf("read-back exif missing link: %s", data)
	}
}

func requireTool(t *testing.T, tool string) {
	t.Helper()
	if _, err := exec.LookPath(tool); err != nil {
		t.Skipf("%s not installed", tool)
	}
}

// sampleWebP renders a tiny solid WebP with ImageMagick for use as sticker input.
func sampleWebP(t *testing.T) []byte {
	t.Helper()
	dir := t.TempDir()
	path := dir + "/sample.webp"
	cmd := exec.Command("magick", "-size", "64x64", "xc:red", path)
	var out strings.Builder
	cmd.Stderr = &out
	if err := cmd.Run(); err != nil {
		t.Fatalf("magick sample webp: %v: %s", err, out.String())
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}
