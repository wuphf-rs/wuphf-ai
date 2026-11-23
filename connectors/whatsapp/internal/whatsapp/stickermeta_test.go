package whatsappconnector

import (
	"bytes"
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
)

// TestStickerExifContainsPackFields checks the EXIF payload carries the pack
// name and folds the project link into the publisher (WhatsApp has no link field
// for received stickers, so the URL rides along as publisher text).
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
		`"sticker-pack-publisher":"wuphf-ai · https://github.com/wuphf/wuphf-ai"`,
	} {
		if !bytes.Contains(exif, []byte(want)) {
			t.Fatalf("exif missing %s\ngot: %s", want, exif)
		}
	}
	if !bytes.HasPrefix(exif, []byte{0x49, 0x49, 0x2A, 0x00}) {
		t.Fatalf("exif missing little-endian TIFF header")
	}
}

// TestStickerExifPublisherWithoutLink leaves the publisher as the bare author
// when no link is configured, so there is no dangling separator.
func TestStickerExifPublisherWithoutLink(t *testing.T) {
	t.Parallel()
	exif, err := stickerExif(config.StickerMetadata{PackName: "p", AuthorName: "a"})
	if err != nil {
		t.Fatalf("stickerExif() error = %v", err)
	}
	if !bytes.Contains(exif, []byte(`"sticker-pack-publisher":"a"`)) {
		t.Fatalf("publisher should be the bare author when no link: %s", exif)
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

// The sticker fixtures are shared with the Rust connector, which writes the same
// EXIF payload and chunk layout without webpmux and checks its bytes against
// these files. WUPHF_UPDATE_FIXTURES=1 regenerates them; otherwise this checks
// that the Go connector still produces them.
var updateFixtures = os.Getenv("WUPHF_UPDATE_FIXTURES") == "1"

func checkFixture(t *testing.T, name string, got []byte) {
	t.Helper()
	path := filepath.Join("testdata", name)
	if updateFixtures {
		if err := os.WriteFile(path, got, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("%s differs from what this connector produces (regenerate with WUPHF_UPDATE_FIXTURES=1)", name)
	}
}

func defaultStickerMetadata() config.StickerMetadata {
	return config.StickerMetadata{
		PackName:   "wuphf",
		AuthorName: "wuphf-ai",
		Link:       "https://github.com/wuphf/wuphf-ai",
	}
}

func TestStickerExifMatchesTheSharedFixture(t *testing.T) {
	t.Parallel()
	exif, err := stickerExif(defaultStickerMetadata())
	if err != nil {
		t.Fatal(err)
	}
	checkFixture(t, "sticker-meta.exif", exif)
	// encoding/json escapes <, > and &; the Rust writer must match.
	escaped, err := stickerExif(config.StickerMetadata{PackName: "Tom & Jerry <3", AuthorName: "wuphf-ai"})
	if err != nil {
		t.Fatal(err)
	}
	checkFixture(t, "sticker-meta-escaped.exif", escaped)
}

func TestStickerMetadataMatchesTheSharedFixtures(t *testing.T) {
	t.Parallel()
	requireTool(t, "webpmux")
	for _, name := range []string{"sticker-lossless-alpha", "sticker-lossy", "sticker-animated"} {
		input, err := os.ReadFile(filepath.Join("testdata", name+".webp"))
		if err != nil {
			t.Fatal(err)
		}
		out, err := writeStickerMetadata(context.Background(), input, defaultStickerMetadata())
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		checkFixture(t, name+".exif.webp", out)
	}
}
