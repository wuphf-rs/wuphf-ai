package mediaconv

import (
	"bytes"
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

// TestToAnimatedWebPFromWebM exercises the WebM->animated-WebP path (Telegram
// video sticker -> WhatsApp sticker), which needs only ffmpeg.
func TestToAnimatedWebPFromWebM(t *testing.T) {
	t.Parallel()
	requireTool(t, "ffmpeg")
	ctx := context.Background()
	webm := buildWebM(t, ctx)

	out, err := ToAnimatedWebP(ctx, webm, ".webm")
	if err != nil {
		t.Fatalf("ToAnimatedWebP() error = %v", err)
	}
	if !isAnimatedWebP(out) {
		t.Fatalf("output is not an animated WebP (%d bytes)", len(out))
	}
}

// TestAnimatedWebPToWebM exercises the animated-WebP->WebM path (WhatsApp
// sticker -> Telegram video sticker). It needs ImageMagick to decode the
// animated WebP (ffmpeg cannot), so it skips when that tool is absent.
func TestAnimatedWebPToWebM(t *testing.T) {
	t.Parallel()
	requireTool(t, "ffmpeg")
	requireImageMagick(t)
	ctx := context.Background()
	webp := buildAnimatedWebP(t, ctx)

	out, err := AnimatedWebPToWebM(ctx, webp)
	if err != nil {
		t.Fatalf("AnimatedWebPToWebM() error = %v", err)
	}
	if frames := webmFrameCount(t, ctx, out); frames < 2 {
		t.Fatalf("expected an animated (multi-frame) WebM, got %d frames", frames)
	}
}

func TestMissingToolErrorReported(t *testing.T) {
	t.Parallel()
	_, err := run(context.Background(), "definitely-not-a-real-tool-xyz")
	var missing *MissingToolError
	if !errors.As(err, &missing) {
		t.Fatalf("error = %v, want MissingToolError", err)
	}
}

func requireTool(t *testing.T, tool string) {
	t.Helper()
	if _, err := exec.LookPath(tool); err != nil {
		t.Skipf("%s not installed", tool)
	}
}

func requireImageMagick(t *testing.T) {
	t.Helper()
	if _, err := imageMagickTool(); err != nil {
		t.Skip("ImageMagick (magick/convert) not installed")
	}
}

// buildWebM renders a short transparent VP9 clip for use as a source video.
func buildWebM(t *testing.T, ctx context.Context) []byte {
	t.Helper()
	dir := t.TempDir()
	out := filepath.Join(dir, "src.webm")
	mustFFmpeg(t, ctx,
		"-f", "lavfi", "-i", "testsrc=size=64x64:rate=10:duration=1",
		"-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", out,
	)
	return readFile(t, out)
}

// buildAnimatedWebP renders a short multi-frame animated WebP source.
func buildAnimatedWebP(t *testing.T, ctx context.Context) []byte {
	t.Helper()
	dir := t.TempDir()
	out := filepath.Join(dir, "src.webp")
	mustFFmpeg(t, ctx,
		"-f", "lavfi", "-i", "testsrc=size=64x64:rate=10:duration=1",
		"-c:v", "libwebp_anim", "-loop", "0", out,
	)
	return readFile(t, out)
}

func webmFrameCount(t *testing.T, ctx context.Context, data []byte) int {
	t.Helper()
	if _, err := exec.LookPath("ffprobe"); err != nil {
		t.Skip("ffprobe not installed")
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "out.webm")
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	out, err := run(ctx, "ffprobe", "-hide_banner", "-loglevel", "error",
		"-select_streams", "v", "-count_frames",
		"-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", path)
	if err != nil {
		t.Fatalf("ffprobe: %v", err)
	}
	n := 0
	for _, b := range bytes.TrimSpace(out) {
		if b < '0' || b > '9' {
			break
		}
		n = n*10 + int(b-'0')
	}
	return n
}

func mustFFmpeg(t *testing.T, ctx context.Context, args ...string) {
	t.Helper()
	full := append([]string{"-hide_banner", "-loglevel", "error", "-y"}, args...)
	if _, err := run(ctx, "ffmpeg", full...); err != nil {
		t.Fatalf("ffmpeg setup failed: %v", err)
	}
}

func readFile(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

// isAnimatedWebP reports whether bytes are a RIFF/WEBP container carrying an
// ANIM chunk, the marker of animation.
func isAnimatedWebP(data []byte) bool {
	if len(data) < 12 || !bytes.Equal(data[0:4], []byte("RIFF")) || !bytes.Equal(data[8:12], []byte("WEBP")) {
		return false
	}
	return bytes.Contains(data, []byte("ANIM"))
}
