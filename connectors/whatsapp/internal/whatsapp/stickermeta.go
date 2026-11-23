package whatsappconnector

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
)

// stickerPackID is the stable identifier WhatsApp uses to group bridged stickers
// into one pack in its sticker info UI.
const stickerPackID = "ai.wuphf.bridge"

// applyStickerMetadata embeds sticker-pack metadata (pack name, author, and a
// link back to the project) into a WebP sticker so WhatsApp attributes it in the
// sticker info panel. It is best-effort: if webpmux is missing or the write
// fails, the original sticker is returned unchanged so the message still sends.
func applyStickerMetadata(ctx context.Context, webp []byte, meta config.StickerMetadata) []byte {
	out, err := writeStickerMetadata(ctx, webp, meta)
	if err != nil {
		return webp
	}
	return out
}

// writeStickerMetadata sets the WhatsApp sticker EXIF chunk on a WebP, returning
// the new bytes. WhatsApp reads pack metadata from an EXIF chunk in a specific
// layout; webpmux writes that chunk verbatim without re-encoding the image.
func writeStickerMetadata(ctx context.Context, webp []byte, meta config.StickerMetadata) ([]byte, error) {
	if _, err := exec.LookPath("webpmux"); err != nil {
		return nil, fmt.Errorf("stickermeta: webpmux not found on PATH")
	}

	exif, err := stickerExif(meta)
	if err != nil {
		return nil, err
	}

	dir, err := os.MkdirTemp("", "wa-stickermeta-*")
	if err != nil {
		return nil, fmt.Errorf("stickermeta: temp dir: %w", err)
	}
	defer os.RemoveAll(dir)

	in := filepath.Join(dir, "in.webp")
	exifFile := filepath.Join(dir, "meta.exif")
	out := filepath.Join(dir, "out.webp")
	if err := os.WriteFile(in, webp, 0o600); err != nil {
		return nil, fmt.Errorf("stickermeta: write sticker: %w", err)
	}
	if err := os.WriteFile(exifFile, exif, 0o600); err != nil {
		return nil, fmt.Errorf("stickermeta: write exif: %w", err)
	}

	cmd := exec.CommandContext(ctx, "webpmux", "-set", "exif", exifFile, in, "-o", out)
	var combined bytes.Buffer
	cmd.Stdout = &combined
	cmd.Stderr = &combined
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("stickermeta: webpmux failed: %w: %s", err, combined.String())
	}
	return os.ReadFile(out)
}

// stickerExif builds the EXIF payload WhatsApp parses for sticker-pack metadata.
// The layout is a minimal little-endian TIFF header carrying a single private
// tag (0x5741, "AW") whose value is a JSON document with the pack fields; this is
// the format WhatsApp's clients expect.
//
// WhatsApp only renders four keys for a received sticker — pack id, name,
// publisher, and emojis — and has no link field (store-link fields exist only in
// the installable-pack API, not in chat stickers). So the project link is folded
// into the publisher text to make it visible (it is not clickable; that is a
// WhatsApp limitation).
func stickerExif(meta config.StickerMetadata) ([]byte, error) {
	publisher := meta.AuthorName
	if meta.Link != "" {
		publisher = strings.TrimSpace(meta.AuthorName + " · " + meta.Link)
	}
	payload := map[string]any{
		"sticker-pack-id":        stickerPackID,
		"sticker-pack-name":      meta.PackName,
		"sticker-pack-publisher": publisher,
		"emojis":                 []string{"🐶"},
	}
	jsonBytes, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("stickermeta: marshal metadata: %w", err)
	}

	var buf bytes.Buffer
	// Little-endian TIFF header, IFD offset 8, one IFD entry: tag 0x5741, type 7
	// (UNDEFINED). The next four bytes are the value's byte count, then its
	// offset (0x16 = 22, where the JSON begins).
	buf.Write([]byte{0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x41, 0x57, 0x07, 0x00})
	length := make([]byte, 4)
	binary.LittleEndian.PutUint32(length, uint32(len(jsonBytes)))
	buf.Write(length)
	buf.Write([]byte{0x16, 0x00, 0x00, 0x00})
	buf.Write(jsonBytes)
	return buf.Bytes(), nil
}
