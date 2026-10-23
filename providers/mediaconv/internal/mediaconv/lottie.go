package mediaconv

import (
	"fmt"

	"github.com/watgbridge/tgsconverter/libtgsconverter"
)

// whatsAppStickerMaxBytes bounds an animated WhatsApp sticker. WhatsApp rejects
// an animated WebP larger than this, so the Lottie raster is retried at lower
// quality and frame rate until it fits.
const whatsAppStickerMaxBytes = 1 << 20 // 1 MiB

// TGSToAnimatedWebP rasterizes a Telegram Lottie sticker (a .tgs file: gzipped
// Lottie JSON) into the animated WebP a WhatsApp sticker requires. ffmpeg and
// ImageMagick cannot render Lottie vector animation, so a vector rasterizer
// (rlottie, via libtgsconverter) draws every frame and encodes them to animated
// WebP. Quality and frame rate are stepped down until the result fits WhatsApp's
// sticker size limit, mirroring how watgbridge handles the same conversion.
//
// The renderer links C/C++ (cgo), which is confined to this provider.
func TGSToAnimatedWebP(tgs []byte) ([]byte, error) {
	opt := libtgsconverter.NewConverterOptions()
	opt.SetExtension("webp")

	var (
		quality float32 = 100
		fps     uint    = 30
	)
	for {
		opt.SetFPS(fps)
		opt.SetWebpQuality(quality)

		webp, err := libtgsconverter.ImportFromData(tgs, opt)
		if err != nil {
			return nil, fmt.Errorf("rasterize Lottie sticker: %w", err)
		}
		if len(webp) > 0 && len(webp) < whatsAppStickerMaxBytes {
			return webp, nil
		}
		if quality <= 2 || fps <= 5 {
			return nil, fmt.Errorf("Lottie sticker exceeds WhatsApp's size limit even at lowest quality")
		}
		quality /= 2
		fps = uint(float32(fps) / 1.5)
	}
}
