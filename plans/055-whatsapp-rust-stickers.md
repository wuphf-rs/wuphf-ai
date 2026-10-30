# Plan 055: whatsapp-rust — stickers: the SDK functions client, media.convert, and a pure-Rust sticker EXIF writer

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Build limits (the owner's laptop)**:
> - Prefix every `cargo` and `go` command with `nice -n 19` and set
>   `CARGO_BUILD_JOBS=2` (Go: `GOMAXPROCS=2 GOFLAGS=-p=2`). Check and test only
>   the crates you touch (`-p <crate>`); never the whole workspace.
> - The whatsapp-rust connector (`connectors/whatsapp-rust`) is its own Cargo
>   workspace and the heaviest build in the repo. Every cargo command for it
>   needs `RUSTC_BOOTSTRAP=1` and `--manifest-path
>   connectors/whatsapp-rust/Cargo.toml`, and it runs alone: never while any
>   other build runs.
> - Before **every** build or test command, run
>   `pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv`.
>   If it prints anything, another build is running: wait, re-checking every
>   60 seconds, until it prints nothing.
> - One build at a time. No `nix flake check`. No release builds except the
>   dev-binary rebuild in the last step.
> - The last step rebuilds the touched dev binaries into
>   `scripts/dev-bin/.bin/`, mirroring `scripts/install-dev-bins.sh`, one at a
>   time and niced, because the owner live-tests with them.
> - Stage explicit paths only (`git add <path> …`; never `git add -A`, `.` or
>   `-u`): another session may be working in the same tree. Run
>   `git status --short` before each commit and commit only this step's files.
>
> **Drift check (run first)**: plans that must be DONE: **054** (and so 051).
> Find 054's last commit with `git log --oneline --grep='mark plan 054 done' -1`
> (`$P052`), then run
> `git log --oneline $P052..HEAD -- crates/wuphf-connector crates/wuphf-proto connectors/whatsapp-rust connectors/shared/functions connectors/whatsapp/internal providers/mediaconv`.
> Expected: no commits, or only commits of plans 052, 053 and 056 that
> `plans/README.md` marks DONE. What must still hold (if any is false, STOP;
> a fact a DONE plan changed as its own plan says counts as holding):
> - 054 is in: `connectors/whatsapp-rust/src/wa_client.rs` has `WaClient`,
>   `OutboundMedia { Image, Video, Audio, Document }` and `LiveClient`;
>   `src/outbound.rs` has `outbound_media`, which sends a sticker as a
>   document (`sticker_file_name`, no caption); `config.rs` has a flattened
>   `Runtime { rendering }`; `crates/wuphf-connector` has
>   `MediaClient::download`.
> - `crates/wuphf-connector` has no functions client; `crates/wuphf-proto/functions.proto:12-21`
>   `FunctionTransport { Invoke(InvokeRequest { FunctionRef function { name,
>   major }; bytes args (UTF-8 JSON); repeated string media_ids; string
>   caller }) returns (InvokeResponse { bytes result; repeated string
>   media_ids; string provider_id }) }`.
> - Go shared (`connectors/shared/functions/`): `client.go:54-77` `Invoke`
>   (bearer token only when set; the deadline is the context's);
>   `mediaconvert.go:10-78` (`media.convert` major 1; args
>   `source_mime_type` / `source_file_name` omitted when empty,
>   `target_mime_type`; result `mime_type`, `file_name`; exactly one result
>   media id, downloaded; the timeout bounds the whole call).
> - Go WhatsApp: `sender.go:444-474` `stickerMessage` (an animated sticker
>   that is not `image/webp` is converted to `image/webp`; a failed
>   conversion sends the original as a document named by
>   `stickerFileName`, no caption; a WebP gets the pack metadata; the sticker
>   carries the mime type and `IsAnimated` from the attachment), `:482-484`,
>   `:513-533`; `stickermeta.go:19` pack id `ai.wuphf.bridge`, `:25-31`
>   best-effort, `:82-109` `stickerExif` (the four keys, the link folded into
>   the publisher as `author · link`, the TIFF header); `config.go:37`
>   `functions_token_env`, `:53` `media_convert_timeout_ms` (0 = 20000),
>   `:76-88` `[sticker_metadata]` and its defaults (`wuphf`, `wuphf-ai`,
>   `https://github.com/wuphf/wuphf-ai`), `:96-117`; `connector.go:62-70,
>   83-85` (functions client with the connector id as caller; converter with
>   a timeout read per call).
> - whatsapp-rust library (`~/.cargo/git/checkouts/whatsapp-rust-6e26428647c827f3/5ca80c5`):
>   no sticker builder in `src/media.rs`; `waproto/src/whatsapp.proto:4371-4394`
>   `StickerMessage { url, file_sha256, file_enc_sha256, media_key, mimetype,
>   height, width, direct_path, file_length, media_key_timestamp, …,
>   is_animated = 13, …, context_info = 17, … }`;
>   `wacore/src/download.rs:59, 72, 88` — `MediaType::Sticker` uses the image
>   key info, `"image"` and `/mms/image`, i.e. it uploads exactly like
>   `MediaType::Image` (which Go uses).
> - The dev shell (`flake.nix:42-50`) provides `magick` (ImageMagick) and
>   `webpmux` (libwebp).

## Status

- **Priority**: P2 — stickers from Telegram reach WhatsApp only as files
  through the Rust connector (054's interim)
- **Effort**: M–L
- **Risk**: MED — a binary-format writer (RIFF/WebP) in the send path; it is
  best-effort, so a bug degrades to a sticker without metadata
- **Depends on**: 054
- **Category**: feature / parity
- **Planned at**: commit `313038d`, 2026-10-31

## Why this matters

A Telegram sticker should arrive on WhatsApp as a sticker, animated when it
moves, and attributed to the bridge's pack in WhatsApp's sticker info, as the
Go connector does. That needs three pieces the Rust side lacks: a client for
the core's remote functions (`media.convert@1` turns a Telegram WebM or Lottie
sticker into an animated WebP), the sticker message itself (whatsapp-rust has
no builder for it), and the sticker-pack EXIF chunk, which the Go connector
writes with the external `webpmux` tool.

Plan 5 of 6 (051–056) bringing the Rust connector to parity with the Go one.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The Rust SDK gains a functions client (Go `functions/client.go` `Invoke`) and the `media.convert@1` contract (Go `functions/mediaconvert.go`), with their own tests. |
| 2 | Config keys `functions_token_env` and `media_convert_timeout_ms` (0 = 20 000 ms), as in Go. |
| 3 | An animated sticker that is not WebP is converted; a failed conversion sends the original as a document, as in Go. |
| 4 | `wa::message::StickerMessage` is filled by hand: the library has no sticker builder. |
| 5 | The sticker EXIF is written by a pure-Rust EXIF chunk writer in the connector, no external binary. Its JSON is exactly Go `stickerExif`'s four keys: pack id, pack name, publisher with the link folded in, emojis (the owner's note `whatsapp-sticker-exif`: WhatsApp reads only these four; there is no link field). |
| 6 | A `[sticker_metadata]` config (`pack_name`, `author_name`, `link`), defaults as in Go. |
| 7 | The RIFF/EXIF bytes are tested against fixtures the Go connector produces. |

### Planner decisions (within the settled scope)

- **The sticker builder takes a connector struct, `UploadedMedia`**, copied
  from the library's `UploadResponse` (its fields are public to read, but
  the type cannot be built outside the library — plan 054's D2). The live
  adapter converts and calls it; tests build `UploadedMedia` directly, so
  the hand-filled proto is tested. Fields filled: url, direct path, media
  key, both SHA-256s, file length, media key timestamp, mime type,
  `is_animated` (Go fills the same, without the media key timestamp; the
  library's own builders fill it for every other kind, so it is kept for
  consistency). No width / height / PNG thumbnail, as in Go.
- **The upload uses `MediaType::Sticker`**: on the wire it is identical to
  `MediaType::Image` (same key info, same `/mms/image` path), which Go uses;
  the sticker type documents the intent.
- **The fixtures live with the reference**, in
  `connectors/whatsapp/internal/whatsapp/testdata/` (Go's convention, as
  `connectors/telegram/testdata`). New Go tests write them when
  `WUPHF_UPDATE_FIXTURES=1` is set and otherwise check that Go still
  produces them, so a change on either side fails a test. The Rust tests
  read them with `include_bytes!`.
- **JSON parity includes Go's escaping**: `encoding/json` escapes `<`, `>`
  and `&` as `<`, `>`, `&` (and U+2028 / U+2029), and its
  map keys come out sorted. The Rust writer serializes a struct with the
  keys in that order and applies the same escapes, so a pack name like
  `Tom & Jerry` produces the same bytes as Go.
- **The EXIF writer is best-effort, like Go's** (`applyStickerMetadata`): any
  error returns the sticker unchanged and is logged at debug.
- **The converter seam**: a `StickerConverter` trait (Go's
  `StickerConverter` interface, `sender.go:27-31`) implemented for the SDK's
  `MediaConverter`, faked in the sender tests.

## The WebP EXIF writer (what the code must do)

A WebP is a RIFF container: `"RIFF"`, a little-endian u32 size (the bytes
after it), `"WEBP"`, then chunks `fourcc, u32 size, payload, a zero pad byte
when size is odd`. To set the EXIF:

- **Extended file** (first chunk `VP8X`, 10-byte payload: one flags byte,
  three reserved bytes, canvas width − 1 and height − 1 as 24-bit
  little-endian): set the EXIF flag (`0x08`), drop any existing `EXIF`
  chunk, insert the new `EXIF` chunk before the first `XMP ` chunk, else at
  the end.
- **Simple file** (first chunk `VP8 ` or `VP8L`): prepend a `VP8X` chunk
  whose canvas is the bitstream's size and whose flags are EXIF, plus ALPHA
  (`0x10`) for a `VP8L` whose header says alpha is used; then the image
  chunk; then the `EXIF` chunk. Bitstream sizes: `VP8 ` — bytes 6–9 of the
  payload after the 3-byte frame tag and the `9d 01 2a` start code hold
  width and height as 14-bit little-endian values; `VP8L` — after the `0x2f`
  signature byte, a little-endian u32 holds width − 1 (14 bits), height − 1
  (14 bits), alpha-is-used (1 bit), version (3 bits).
- Rewrite the RIFF size. Anything that does not parse (not RIFF/WEBP, a
  truncated chunk, an unknown first chunk) is an error.

This is what `webpmux -set exif` produces; Step 3's fixtures check it byte
for byte.

## Current state

- After 054, `outbound_media` maps `KIND_STICKER` to a document; the sender
  has no converter; `config.rs` `Runtime` has only `rendering`.
- The core brokers `media.convert@1` to the `providers/mediaconv` process
  (plan 033); the Go connector dials it through
  `functions.Dial(core_address, token, connector_id)`, with the token from
  `functions_token_env` when set (none when the core sets no
  `[functions] invoke_token_env`).
- Go's tests here: `stickermeta_test.go` (`TestStickerExifContainsPackFields`,
  `TestStickerExifPublisherWithoutLink`, `TestApplyStickerMetadataDegrades`,
  `TestWriteStickerMetadataRoundTrips`), `sender_test.go`
  (`TestAnimatedStickerIsConvertedToWebP`, `TestTGSStickerIsConvertedToWebP`,
  `TestStickerConversionFailureFallsBackToDocument`,
  `TestStaticWebPStickerSkipsConversion`), `runtime_settings_test.go`
  (`TestStickerMetadataChangeAppliesToNextSticker`, for 056),
  `connectors/shared/functions/functions_test.go`.

## Commands you will need

```sh
pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv   # must print nothing

export CARGO_BUILD_JOBS=2
nice -n 19 cargo test -p wuphf-connector
nice -n 19 cargo clippy -p wuphf-connector --all-targets -- -D warnings
cargo fmt --check

(cd connectors/whatsapp && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go vet ./... && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go test ./internal/whatsapp/)

RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo test --manifest-path connectors/whatsapp-rust/Cargo.toml
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --manifest-path connectors/whatsapp-rust/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path connectors/whatsapp-rust/Cargo.toml --check
```

## Scope

In: `crates/wuphf-connector/{Cargo.toml,src/lib.rs,src/functions.rs (new),tests/functions.rs (new)}`,
`connectors/whatsapp/internal/whatsapp/{stickermeta_test.go,testdata/ (new fixtures)}`,
`connectors/whatsapp-rust/{Cargo.toml (only if a dependency edge is needed),config.example.toml,src/main.rs,src/config.rs,src/sender.rs,src/outbound.rs,src/wa_client.rs,src/stickermeta.rs (new)}`,
`README.md` (Rust connector section), `plans/README.md`.

Out: live settings (056 makes `sticker_metadata` and the timeout live), the
Go connector's code (only a test and fixtures are added), the mediaconv
provider, the core, the proto, `.env.local`, `config.local.toml`,
`*.session.json`, `.claude/`, `connectors/instagram/**`, `plans/048-*`.

## Git workflow

Commit directly on `v2`, one commit per step. Do not push. No branches.

## Steps

### Step 0: Baseline

Drift check. Record `cargo test -p wuphf-connector`, the whatsapp-rust test
count (alone), and `go test ./internal/whatsapp/` for `connectors/whatsapp`
(one module).

### Step 1: SDK functions client and media.convert

`crates/wuphf-connector/Cargo.toml`: add `serde.workspace = true` (with
`derive`, as the workspace defines it) and `serde_json.workspace = true`.
Both are already in both lock files (whatsapp-rust re-exports `serde_json`),
so only the dependency edges change.

New `crates/wuphf-connector/src/functions.rs`, exported from `lib.rs`:

```rust
//! Calls the core's remote functions: the counterpart of the Go
//! `connectors/shared/functions` client, and the `media.convert@1`
//! contract callers and providers share.

pub const MEDIA_CONVERT: &str = "media.convert";
pub const MEDIA_CONVERT_MAJOR: u32 = 1;

/// The args of media.convert@1; the source is the call's one media id.
#[derive(Debug, Serialize)]
pub struct MediaConvertArgs {
    #[serde(skip_serializing_if = "String::is_empty")] pub source_mime_type: String,
    #[serde(skip_serializing_if = "String::is_empty")] pub source_file_name: String,
    pub target_mime_type: String,
}
/// The result of media.convert@1; the converted media is the one result id.
#[derive(Debug, Deserialize)]
pub struct MediaConvertResult { pub mime_type: String, pub file_name: String }

#[derive(Debug, thiserror::Error)]
pub enum FunctionError {
    #[error("invoke {function}: encode args: {source}")] Encode { function: String, source: serde_json::Error },
    #[error("invoke {function}: {source}")] Call { function: String, source: tonic::Status },
    #[error("invoke {function}: decode result from provider {provider}: {source}")] Decode { function: String, provider: String, source: serde_json::Error },
    #[error("{function} returned {count} media ids, want 1")] MediaCount { function: String, count: usize },
    #[error("download converted media: {0}")] Download(SessionError),
    #[error("{function} timed out after {after:?}")] Timeout { function: String, after: Duration },
    #[error(transparent)] Setup(SessionError),
}

/// A client of the core's FunctionTransport. Cheap to clone.
#[derive(Clone)]
pub struct FunctionsClient { /* client, Option<authorization>, caller */ }
impl FunctionsClient {
    /// Lazy connection; `token` is the core's invoke token (none when the
    /// core requires none); `caller` labels this process in the core's logs.
    pub fn connect(core_address: &str, token: Option<&str>, caller: impl Into<String>) -> Result<Self, SessionError>;
    /// Call name@major with JSON `args` and `media_ids`; decode the JSON
    /// result; return it with the result media ids. `deadline` becomes the
    /// call's gRPC timeout, which the core honours while it waits for a
    /// provider.
    pub async fn invoke<A: Serialize, R: DeserializeOwned>(
        &self, name: &str, major: u32, args: &A, media_ids: Vec<String>, deadline: Option<Duration>,
    ) -> Result<(R, Vec<String>), FunctionError>;
}

/// Converts stored media through media.convert@1 and downloads the result.
/// `timeout` is read on every call (Go's `TimeoutFunc`), so a timeout that
/// is a runtime setting applies to the next conversion; it bounds the whole
/// call, waiting for a provider and the download included.
#[derive(Clone)]
pub struct MediaConverter {
    pub functions: FunctionsClient,
    pub media: MediaClient,
    pub timeout: Arc<dyn Fn() -> Duration + Send + Sync>,
}
impl MediaConverter {
    pub async fn convert(&self, media_id: &str, source_mime: &str, source_name: &str, target_mime: &str)
        -> Result<Vec<u8>, FunctionError>;
}
```

`thiserror` is already a dependency of the SDK. Use
`tonic::Request::set_timeout` for the deadline and `tokio::time::timeout`
around the whole `convert`.

Tests (`crates/wuphf-connector/tests/functions.rs`): a fake
`FunctionTransport` server written in the test (implement
`wuphf_proto::connector::function_transport_server::FunctionTransport`:
`invoke` records the request and its `authorization` header and answers
from a scripted reply, optionally after a delay; `list_functions` returns
`Status::unimplemented`), served together with the real media service of
`tests/media.rs` (`GrpcMediaService` behind `any_token_auth`) on one
loopback `Server`:

1. `invoke_sends_json_args_media_and_caller` — name, major, args JSON,
   media ids and caller arrive; `authorization: Bearer t` with a token,
   **no** header without one (Go `client.go:59-61`).
2. `invoke_decodes_the_result` — result JSON → `R`, media ids returned.
3. `an_undecodable_result_names_the_provider` → `Decode { provider, .. }`.
4. `media_convert_args_encode_like_go` — empty source fields are omitted:
   `{"target_mime_type":"image/webp"}`; full args in field order.
5. `convert_downloads_the_converted_media` — upload `b"webp"` to the media
   service, script the fake to return its id → `convert` returns `b"webp"`,
   and the fake saw `media.convert@1` with the source media id.
6. `convert_wants_exactly_one_result` — two ids → `MediaCount`.
7. `convert_honours_its_timeout` — the fake answers after 2 s, timeout
   100 ms → `Timeout` (or `Call` with `DeadlineExceeded`) within 1 s.

**Verify**: `nice -n 19 cargo test -p wuphf-connector` → Step 0 count + 7;
clippy clean; `git diff Cargo.lock` shows only `wuphf-connector`'s
dependency list. Mutation: always send the bearer header → test 1 fails.
Restore.
Commit: `feat(connector-sdk): call remote functions and media.convert`.

### Step 2: Fixtures from the Go connector

1. Inputs (inside `nix develop`, from
   `connectors/whatsapp/internal/whatsapp/testdata/`):
   ```sh
   magick -size 5x3 xc:'#ff000080' -define webp:lossless=true sticker-lossless-alpha.webp
   magick -size 5x3 xc:'#00ff00' -quality 50 sticker-lossy.webp
   magick -delay 10 -size 5x3 xc:red xc:blue -loop 0 sticker-animated.webp
   for f in sticker-*.webp; do printf '%s ' "$f"; xxd -s 12 -l 4 -p "$f" | xxd -r -p; echo; done
   ```
   The first chunk must be `VP8L` for the lossless file, `VP8 ` for the
   lossy one and `VP8X` for the animated one. If ImageMagick produces a
   different layout, try `cwebp` (`cwebp -lossless` / `cwebp -q 50` on a PNG
   made by `magick`) and `img2webp` for the animated file; if no tool yields
   all three layouts, STOP.
2. `stickermeta_test.go`, two new tests (the module's existing helpers
   `requireTool` and `writeStickerMetadata`):
   - `TestStickerExifMatchesTheSharedFixture`: `stickerExif` of the default
     metadata (`wuphf`, `wuphf-ai`, `https://github.com/wuphf/wuphf-ai`) and
     of `{PackName: "Tom & Jerry <3", AuthorName: "wuphf-ai", Link: ""}`;
     with `WUPHF_UPDATE_FIXTURES=1` write them to `testdata/sticker-meta.exif`
     and `testdata/sticker-meta-escaped.exif`, otherwise compare byte for
     byte.
   - `TestStickerMetadataMatchesTheSharedFixtures`: for each of the three
     inputs, `writeStickerMetadata(ctx, input, defaults)` (webpmux); write
     `testdata/<name>.exif.webp` when updating, otherwise compare. Skips
     without `webpmux` like the existing round-trip test.
3. Generate: `(cd connectors/whatsapp && WUPHF_UPDATE_FIXTURES=1 GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go test ./internal/whatsapp/ -run 'MatchesTheShared')`,
   then run the same without the variable → both pass (the module's whole
   test package too).

**Verify**: `git status --short connectors/whatsapp` → only
`stickermeta_test.go` modified and the 8 new files under `testdata/` (3
inputs, 3 outputs, 2 EXIF payloads); `go vet ./...` and `go test
./internal/whatsapp/` green.
Commit: `test(whatsapp): sticker metadata fixtures shared with the Rust connector`.

### Step 3: The pure-Rust EXIF writer

New `connectors/whatsapp-rust/src/stickermeta.rs`:

```rust
//! Sticker-pack metadata, written into a WebP sticker as WhatsApp expects:
//! an EXIF chunk holding a little-endian TIFF block with one private tag
//! (0x5741, "AW") whose value is a JSON document. WhatsApp shows only four
//! keys for a received sticker, and has no link field, so the link is folded
//! into the publisher (visible, not clickable). Byte-compatible with the Go
//! connector's `stickerExif` and with `webpmux -set exif`, without the tool.

pub const STICKER_PACK_ID: &str = "ai.wuphf.bridge";

/// The EXIF payload (Go `stickerExif`).
pub fn sticker_exif(meta: &StickerMetadata) -> Vec<u8>;

/// `webp` with its EXIF chunk set to `exif` (see the plan's "WebP EXIF
/// writer" section).
pub fn set_exif(webp: &[u8], exif: &[u8]) -> Result<Vec<u8>, WebpError>;

/// Best-effort: the sticker with its metadata, or unchanged on any error.
pub fn apply_sticker_metadata(webp: Vec<u8>, meta: &StickerMetadata) -> Vec<u8>;

#[derive(Debug)] pub enum WebpError { NotWebp, Truncated, UnknownLayout(String) }  // Display by hand
```

The JSON: a `#[derive(Serialize)]` struct with fields in this order —
`emojis` (`["🐶"]`), `sticker-pack-id`, `sticker-pack-name`,
`sticker-pack-publisher` (`author · link`, trimmed, or the author alone
when there is no link) — serialized with `serde_json::to_vec`, then Go's
HTML escapes applied to the bytes (`<` → `<`, `>` → `>`, `&` →
`&`, U+2028 → ` `, U+2029 → ` `; safe as a blind replace,
these characters only occur inside strings). The header: `49 49 2A 00 08 00
00 00 01 00 41 57 07 00`, the JSON length as a little-endian u32, `16 00 00
00`, the JSON.

`StickerMetadata` comes from Step 4's config; for this step, define it in
`config.rs` already (`pack_name`, `author_name`, `link`, all `String`).

Tests (`stickermeta.rs`; the fixtures through
`include_bytes!("../../whatsapp/internal/whatsapp/testdata/…")`):

1. `the_exif_matches_go` — defaults → `sticker-meta.exif`.
2. `the_exif_escapes_like_go` — `Tom & Jerry <3`, no link →
   `sticker-meta-escaped.exif` (the publisher is the author alone).
3. `each_layout_matches_webpmux` — for the lossless, lossy and animated
   inputs, `set_exif(input, sticker_exif(defaults))` equals the matching
   `.exif.webp`.
4. `setting_exif_twice_replaces_it` — applying to an output again (with other
   metadata) leaves exactly one `EXIF` chunk with the new payload.
5. `an_odd_payload_is_padded` — a 3-byte EXIF → the chunk has one zero pad
   byte and the RIFF size counts it.
6. `garbage_is_refused_and_left_alone` — `b"not a webp"` → `set_exif` errors;
   `apply_sticker_metadata` returns the input unchanged (Go
   `TestApplyStickerMetadataDegrades`).
7. `a_truncated_chunk_is_refused`.

**Verify**: whatsapp-rust tests → Step 0 count + 7, green. Mutation: leave
out the ALPHA flag for VP8L → test 3 fails on the lossless input; skip the
HTML escapes → test 2 fails. Restore.
Commit: `feat(whatsapp-rust): write sticker-pack EXIF in pure Rust`.

### Step 4: Send stickers

1. `config.rs` (Go `config.go`):
   - `Config.functions_token_env: Option<String>` (`#[serde(default)]`).
   - `Runtime` gains `#[serde(default)] media_convert_timeout_ms: u32` and
     `#[serde(default, rename = "sticker_metadata")] sticker_metadata:
     StickerMetadata`; methods `media_convert_timeout() -> Duration` (0 →
     20 s) and `sticker_metadata() -> StickerMetadata` (blank fields → the
     defaults), as Go `MediaConvertTimeout` / `StickerMetadata`.
2. `wa_client.rs`: `OutboundMedia::Sticker(StickerOptions { mimetype:
   Option<String>, is_animated: bool })`; the live adapter uploads it with
   `MediaType::Sticker` and builds it with `outbound::sticker_message(
   UploadedMedia::from(&upload), options)`.
3. `outbound.rs`:
   ```rust
   /// What an upload returned, as the connector's own value (the library's
   /// `UploadResponse` cannot be built outside it, so builders the
   /// connector writes take this instead).
   pub struct UploadedMedia { pub url: String, pub direct_path: String, pub media_key: [u8; 32],
       pub file_enc_sha256: [u8; 32], pub file_sha256: [u8; 32], pub file_length: u64, pub media_key_timestamp: i64 }
   impl From<&UploadResponse> for UploadedMedia { … }

   /// A sticker message, filled by hand: whatsapp-rust has no sticker builder.
   pub fn sticker_message(upload: UploadedMedia, options: StickerOptions) -> wa::Message;
   ```
4. `sender.rs`: a `StickerConverter` trait (`convert(media_id, source_mime,
   source_name, target_mime) -> anyhow::Result<Vec<u8>>`) implemented for
   `wuphf_connector::MediaConverter`; the sender holds one. Sticker path
   (Go `stickerMessage`), taken when the kind is `KIND_STICKER` and
   `attachments_as_files` is off:
   - animated and mime ≠ `image/webp` → convert to `image/webp`; on error,
     `warn!` and send the **original** bytes as a document named
     `sticker_file_name(attachment)` with no caption;
   - mime `image/webp` (originally or after conversion) →
     `apply_sticker_metadata(bytes, &runtime.sticker_metadata())`;
   - upload as `OutboundMedia::Sticker { mimetype, is_animated:
     attachment.animated }`.
   The reply context and the timer apply as for any part (054).
   `outbound_media` no longer handles stickers except under
   `attachments_as_files`.
5. `main.rs`: `FunctionsClient::connect(&config.core_address,
   functions_token.as_deref(), &config.connector_id)` with the token read
   from `functions_token_env` when set (empty → none); `MediaConverter {
   functions, media: media_client.clone(), timeout: Arc::new(move ||
   runtime.media_convert_timeout()) }` (056 makes it read the live value).
6. `config.example.toml`: the Go example's functions / timeout comment and
   the `[sticker_metadata]` block, **without** the "Requires webpmux"
   sentence (this connector needs no tool).

Tests (`sender.rs` with a `FakeConverter { result: Result<Vec<u8>, String>,
calls: Mutex<Vec<(String, String, String, String)>> }`; `outbound.rs`;
`config.rs`):

1. `an_animated_webm_sticker_is_converted` — mime `video/webm`, animated →
   the converter got `(media id, video/webm, file name, image/webp)`; a
   `Sticker` upload with `image/webp`, `is_animated: true`, and the uploaded
   bytes contain `"sticker-pack-name":"wuphf"` (Go
   `TestAnimatedStickerIsConvertedToWebP`).
2. `a_lottie_sticker_is_converted` — `application/x-tgsticker` (Go
   `TestTGSStickerIsConvertedToWebP`).
3. `a_failed_conversion_sends_the_original_as_a_file` — converter errors →
   a `Document` upload of the original bytes named `sticker.webm`, no
   caption (Go `TestStickerConversionFailureFallsBackToDocument`).
4. `a_static_webp_sticker_is_not_converted` — not animated, `image/webp` →
   no converter call, metadata applied (Go
   `TestStaticWebPStickerSkipsConversion`).
5. `the_configured_pack_metadata_is_used` — runtime with `pack_name =
   "mine"` → the bytes carry `"sticker-pack-name":"mine"`.
6. `a_sticker_carries_the_timer_and_the_reply` — context and `expiration`
   land on `sticker_message`.
7. `files_mode_sends_stickers_as_files` — `attachments_as_files` → a
   document, no conversion.
8. `the_sticker_message_is_filled_by_hand` (`outbound.rs`) — every
   `UploadedMedia` field and both options land in `StickerMessage`.
9. `sticker_settings_default_like_go` (`config.rs`) — blank fields → the
   three defaults; `media_convert_timeout_ms = 0` → 20 s, 5000 → 5 s.

**Verify**: whatsapp-rust tests → + 9, green; clippy clean;
`git diff --stat 313038d -- connectors/whatsapp-rust/Cargo.lock` shows only
dependency-edge changes (`serde_json` for `wuphf-connector`), no new
package. Mutation: convert static WebP stickers too → test 4 fails; apply
metadata only before conversion → test 1 fails. Restore.
Commit: `feat(whatsapp-rust): send stickers, converted and with pack metadata`.

### Step 5: Docs

`README.md`, Rust connector section: stickers go out as stickers (animated
ones converted by the `media.convert` provider, `providers/mediaconv`; files
when it is unavailable), with the configured pack metadata, written without
external tools; the `functions_token_env`, `media_convert_timeout_ms` and
`[sticker_metadata]` keys.

Commit: `docs(whatsapp-rust): stickers`.

### Step 6: Gate, dev binary, index, and the owner's live test

1. Gate: `cargo test -p wuphf-connector`, clippy, `cargo fmt --check`;
   `connectors/whatsapp` `go vet ./...` and `go test ./...` (one module);
   alone, the whatsapp-rust test, clippy, fmt.
2. Dev binary (check `pgrep` first): the Go WhatsApp binary is unchanged
   (only tests and fixtures were added), so only:
   ```sh
   RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo build --release --manifest-path connectors/whatsapp-rust/Cargo.toml
   install -m 0755 connectors/whatsapp-rust/target/release/wuphf-connector-whatsapp scripts/dev-bin/.bin/wuphf-whatsapp-rust
   ```
3. `plans/README.md`: row 055 DONE with notes. Commit:
   `docs(plans): mark plan 055 done`.

**For the owner:**

- **Restart**: the whatsapp-rust connector (`wwr`). The sticker converter
  (`wmc`, `providers/mediaconv`) must be running for animated stickers; the
  core is unchanged.
- **Config** (the connector's own file): `functions_token_env` (only when the
  core sets `[functions] invoke_token_env`; the same variable must be
  exported for the connector), `media_convert_timeout_ms` (default 20000),
  `[sticker_metadata]` `pack_name` / `author_name` / `link` (defaults
  `wuphf` / `wuphf-ai` / `https://github.com/wuphf/wuphf-ai`).
- **Live test** (from Telegram into a chat mirrored to this connector):
  1. Send a static sticker → a WhatsApp sticker. Long-press it → sticker
     info shows pack `wuphf`, publisher `wuphf-ai · https://github.com/wuphf/wuphf-ai`.
  2. Send an animated (video) sticker and an animated Lottie (`.tgs`)
     sticker → animated WhatsApp stickers.
  3. Stop `wmc`, set `media_convert_timeout_ms = 5000`, restart `wwr`, send
     an animated sticker → after about 5 s it arrives as a file
     (`sticker.webm` or its own name). Start `wmc` again and put the timeout
     back.
  4. Reply from Telegram to a WhatsApp message with a sticker → the sticker
     arrives quoting it; in a disappearing-messages chat it shows no "will
     not disappear" mark.

## Test plan

| Test | Where | Proves |
|---|---|---|
| invoke args / auth / decode / provider name, args encoding, convert download / one result / timeout | wuphf-connector | decision 1 |
| Go writes and checks the shared fixtures | Go whatsapp | decision 7 |
| EXIF bytes, escaping, three layouts vs webpmux, replace, padding, garbage, truncation | whatsapp-rust stickermeta | decisions 5, 7 |
| webm / tgs conversion, fallback document, static WebP, configured metadata, timer and reply, files mode, hand-filled proto, config defaults | whatsapp-rust sender, outbound, config | decisions 2–4, 6 |

## Done criteria

- [ ] Every test above passes; every listed mutation fails a test.
- [ ] The Go module's tests check the same fixtures the Rust tests read.
- [ ] No new package in either lock file; clippy, fmt and `go vet` clean.
- [ ] Dev binary installed; row 055 DONE with notes and the owner's steps.

## STOP conditions

- The drift check fails, 054 is not DONE, or a "must still hold" fact is
  false.
- No tool in the dev shell produces all three input layouts (Step 2).
- `set_exif` cannot be made byte-identical to `webpmux` for one of the three
  layouts: report the first differing offset and both chunk lists; do not
  weaken the test on your own.
- Adding `serde` / `serde_json` to the SDK adds a package to either lock
  file (a version mismatch).
- Any test that existed at Step 0 starts failing for a reason this plan does
  not explain.

## Maintenance notes

- **The EXIF fixtures are shared**: the Go test regenerates them with
  `WUPHF_UPDATE_FIXTURES=1`; after changing `stickerExif` or its defaults on
  either side, regenerate and run both suites.
- **The Go connector still needs `webpmux`**; the Rust one needs no tool.
  The owner's note `whatsapp-sticker-exif` describes the format both write.
- **The sticker builder is the connector's own** (`sticker_message`). If
  whatsapp-rust gains one, switch to it and keep the test on the fields.
- **The converter timeout bounds the whole sticker delivery**, and this
  connector delivers in order, so later messages wait behind a slow
  conversion (as in Go).
- **Animated WebP decoding is the provider's concern** (the owner's note
  `animated-webp-decode`: ImageMagick, not ffmpeg); nothing here decodes
  images.
