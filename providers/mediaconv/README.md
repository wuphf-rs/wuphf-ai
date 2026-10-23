# mediaconv provider

`wuphf-mediaconv` converts animated stickers between the formats chat platforms
use. It is a remote-function provider: it dials the core, registers
`media.convert@1`, and serves the conversions connectors ask the core for.
Connectors carry no conversion code or tools of their own.

## The `media.convert@1` contract

The call carries the source as its single media id and these args (JSON):

| Field | Meaning |
|-------|---------|
| `source_mime_type` | MIME type of the source, e.g. `video/webm`. |
| `source_file_name` | Its file name; the extension is used when the MIME type does not settle the format. |
| `target_mime_type` | Required. `video/webm` or `image/webp`. |

The result's single media id is the converted sticker, uploaded to the core's
media store as an animated sticker, and its body is
`{"mime_type": "...", "file_name": "..."}`. The JSON Schemas in
`internal/convert/handler.go` are the reference for both bodies.

Supported conversions:

| Source | Target | Tool | Used by |
|--------|--------|------|---------|
| animated WebP (WhatsApp) | `video/webm` | ImageMagick decodes to APNG, ffmpeg encodes VP9 (512 px, 3 s) | Telegram |
| Lottie `.tgs` (Telegram) | `image/webp` | rlottie renderer, stepped down to fit 1 MiB | WhatsApp |
| any ffmpeg-decodable video (Telegram WebM) | `image/webp` | ffmpeg | WhatsApp |

Any other pair fails with `UNSUPPORTED`; a missing tool or a failed conversion
fails with `FAILED`. The caller then falls back (Telegram sends a static
sticker, WhatsApp sends a file). The function is declared idempotent, so a call
lost when the provider disconnects is re-queued for the next provider.

## Requirements

- At run time: `ffmpeg` and ImageMagick (`magick` or `convert`) on `PATH`. The
  nix dev shell provides both.
- At build time: cgo and a C/C++ toolchain, for the rlottie renderer. This
  provider is the only Go module that needs them for stickers.

## Configuration

Both sides must agree. In the core configuration:

```toml
[functions]
[[functions.providers]]
id = "mediaconv"
token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
functions = ["media.convert"]
```

Then copy `config.example.toml`: its `provider_id` must match the core's `id`
and its `token_env` names a variable holding the same token the core reads.

```sh
export WUPHF_PROVIDER_MEDIACONV_TOKEN=replace-me
go run ./cmd/wuphf-mediaconv -config config.example.toml
```

The provider reconnects on its own when the core restarts. To check it
registered:

```sh
cargo run -p wuphf-cli -- functions list
# media.convert@1  provider=mediaconv  in_flight=0/8  queued=0  ...
```

If you do not run this provider, remove its `[[functions.providers]]` entry
from the core configuration. Otherwise every animated sticker waits for a
converter until the connector's `media_convert_timeout_ms` before falling back,
and that connector's later messages wait behind it.
