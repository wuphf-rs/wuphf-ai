// Package mediaconv transcodes sticker media between the formats different chat
// platforms use, so an animated sticker keeps moving when it crosses a bridge.
//
// Stickers are the only media that needs transcoding: a GIF is an MP4 with a
// playback flag on every platform involved, so it is carried as-is and only the
// flag is set by the sender. Animated stickers, however, use incompatible
// container/codec pairs — WhatsApp uses animated WebP, Telegram uses WebM (VP9)
// video stickers — and neither side renders the other's format, so the bytes
// must be re-encoded.
//
// The conversions shell out to ffmpeg and ImageMagick rather than linking a
// codec library, keeping the connectors cgo-free. ffmpeg cannot decode animated
// WebP (upstream ticket 4907 is unmerged as of 2025), and libwebp's CLI tools
// ship no animated decoder in common packages, so ImageMagick decodes the
// animated WebP to an APNG — preserving alpha and per-frame timing — which ffmpeg
// re-encodes to WebM. The reverse direction is pure ffmpeg.
package mediaconv

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
)

// Telegram video stickers must be WebM/VP9, at most 512x512, and short. These
// bound the transcode so the result is accepted as a sticker rather than a video.
const (
	stickerMaxEdge    = 512
	stickerMaxSeconds = 3
	// stickerMaxBitrate caps the average rate so a 3s WebM stays under Telegram's
	// 256KB sticker limit (600k * 3s ~= 225KB) while leaving enough headroom over
	// the old 500k to avoid the low-bitrate banding VP9 produced on busy stickers.
	stickerMaxBitrate = "600k"
)

// MissingToolError reports that a required external binary is not on PATH, so a
// caller can degrade gracefully (e.g. send the sticker as a still image) instead
// of failing the whole message.
type MissingToolError struct{ Tool string }

func (e *MissingToolError) Error() string {
	return fmt.Sprintf("mediaconv: required tool %q not found on PATH", e.Tool)
}

// AnimatedWebPToWebM transcodes an animated WebP (a WhatsApp animated sticker)
// into a WebM/VP9 video sticker for Telegram, preserving transparency. ffmpeg
// cannot decode animated WebP, so ImageMagick first decodes it to an APNG —
// which carries every composited frame, the alpha channel, and the per-frame
// timing — and ffmpeg re-encodes that to WebM at the source's playback speed.
func AnimatedWebPToWebM(ctx context.Context, src []byte) ([]byte, error) {
	dir, err := os.MkdirTemp("", "mediaconv-webp2webm-*")
	if err != nil {
		return nil, fmt.Errorf("mediaconv: temp dir: %w", err)
	}
	defer os.RemoveAll(dir)

	input := filepath.Join(dir, "in.webp")
	if err := os.WriteFile(input, src, 0o600); err != nil {
		return nil, fmt.Errorf("mediaconv: write input: %w", err)
	}

	magick, err := imageMagickTool()
	if err != nil {
		return nil, err
	}
	// -coalesce flattens an animated WebP's frame disposal: WhatsApp stickers
	// store each frame as a sub-rectangle patch over the previous one, and
	// without coalescing those partial frames composite wrong (smearing and
	// banding). -define png:color-type=6 forces an RGBA APNG so transparency is
	// carried through; ImageMagick otherwise writes an opaque RGB APNG, dropping
	// the sticker's alpha.
	apng := filepath.Join(dir, "frames.apng")
	if _, err := run(ctx, magick,
		input, "-coalesce", "-define", "png:color-type=6", apng,
	); err != nil {
		return nil, err
	}

	output := filepath.Join(dir, "out.webm")
	if _, err := run(ctx, "ffmpeg",
		"-hide_banner", "-loglevel", "error", "-y",
		"-f", "apng", "-i", apng,
		"-an",
		"-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p",
		// libvpx-vp9 encodes the alpha plane as a second stream; alt-ref and
		// look-ahead frames desync it from the color stream, which shows up as
		// the sweeping vertical bands seen on the mirror. Disabling both keeps
		// the planes frame-aligned.
		"-auto-alt-ref", "0", "-lag-in-frames", "0",
		"-vf", stickerScaleFilter,
		"-t", strconv.Itoa(stickerMaxSeconds),
		// Telegram rejects a video sticker over 256KB, so cap the bitrate
		// (VP9 constrained quality): -crf governs quality, -b:v bounds the
		// average rate so a short sticker stays well under the limit.
		"-crf", "30", "-b:v", stickerMaxBitrate,
		output,
	); err != nil {
		return nil, err
	}
	return os.ReadFile(output)
}

// imageMagickTool resolves the ImageMagick CLI, preferring the v7 `magick`
// command and falling back to the legacy `convert`. A missing tool is reported
// so callers can degrade gracefully.
func imageMagickTool() (string, error) {
	for _, tool := range []string{"magick", "convert"} {
		if _, err := exec.LookPath(tool); err == nil {
			return tool, nil
		}
	}
	return "", &MissingToolError{Tool: "magick"}
}

// ToAnimatedWebP transcodes a video sticker (a Telegram WebM/VP9 sticker, or any
// ffmpeg-decodable animation) into the animated WebP a WhatsApp sticker requires,
// scaled into WhatsApp's 512x512 sticker box and preserving transparency. srcExt
// is the source container extension (e.g. ".webm", ".mp4") so ffmpeg demuxes it.
func ToAnimatedWebP(ctx context.Context, src []byte, srcExt string) ([]byte, error) {
	dir, err := os.MkdirTemp("", "mediaconv-towebp-*")
	if err != nil {
		return nil, fmt.Errorf("mediaconv: temp dir: %w", err)
	}
	defer os.RemoveAll(dir)

	if srcExt == "" {
		srcExt = ".bin"
	}
	input := filepath.Join(dir, "in"+srcExt)
	if err := os.WriteFile(input, src, 0o600); err != nil {
		return nil, fmt.Errorf("mediaconv: write input: %w", err)
	}
	output := filepath.Join(dir, "out.webp")
	if _, err := run(ctx, "ffmpeg",
		"-hide_banner", "-loglevel", "error", "-y",
		"-i", input,
		"-an",
		"-c:v", "libwebp_anim", "-loop", "0", "-pix_fmt", "yuva420p",
		"-vf", stickerScaleFilter,
		"-t", strconv.Itoa(stickerMaxSeconds),
		output,
	); err != nil {
		return nil, err
	}
	return os.ReadFile(output)
}

// stickerScaleFilter fits the media inside the platform sticker box without
// upscaling or distorting it: it shrinks the longest edge to stickerMaxEdge only
// when larger, preserving aspect ratio (the other edge stays free).
// force_divisible_by=2 rounds both edges to even numbers, which yuv420p chroma
// subsampling requires; an odd edge otherwise yields a half-filled chroma plane
// that renders as banding.
var stickerScaleFilter = fmt.Sprintf(
	"scale='min(%d,iw)':'min(%d,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
	stickerMaxEdge, stickerMaxEdge,
)

// run executes a tool, returning its combined output. A missing binary is
// reported as MissingToolError so callers can fall back; any other failure
// includes the tool's stderr to aid debugging.
func run(ctx context.Context, tool string, args ...string) ([]byte, error) {
	if _, err := exec.LookPath(tool); err != nil {
		return nil, &MissingToolError{Tool: tool}
	}
	var out bytes.Buffer
	cmd := exec.CommandContext(ctx, tool, args...)
	cmd.Stdout = &out
	cmd.Stderr = &out
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("mediaconv: %s failed: %w: %s", tool, err, strings.TrimSpace(out.String()))
	}
	return out.Bytes(), nil
}
