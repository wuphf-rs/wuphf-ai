# Implementation Status

This document records current workspace changes and next steps.

## Current State

The repository now contains a Rust core runtime plus Go Telegram and WhatsApp
connector work. The `ReplyContext` refactor is complete and the full workspace
passes format, lint, test, and `nix flake check`.

## Core Runtime Changes

- Added core-owned media identity with `MediaId`.
- Replaced platform media URLs in `Attachment` with:
  - `media_id`
  - `kind`
  - optional `file_name`
  - optional `mime_type`
  - optional `size_bytes`
- Added `MediaBlob` for core-owned bytes.
- Added `MediaTransport` gRPC service:
  - `UploadMedia`
  - `DownloadMedia`
- Added runtime media service in `crates/wuphf-runtime/src/media.rs`.
- Mounted `MediaTransport` in CLI server mode.
- Added `max_media_bytes` config.
- Added `orphan_media_retention_seconds` config.
- Added upload size validation.
- Added upload metadata validation.
- Added media download streaming in chunks.
- Added media upload pruning for expired unreferenced uploads.
- Added core validation that inbound attachments must reference uploaded media
  and match broker-issued metadata before event is accepted.
- Kept routing, dedup, echo suppression, reply identity, and persistence in
  Rust core.

## Storage Changes

- Extended `Storage` trait with:
  - `put_media`
  - `media`
  - `prune_unreferenced_media_before`
- Added in-memory media storage.
- Added SQLite media blob persistence.
- Added SQLite media reference tracking from logical messages.
- Added migration `migrations/0002_media_blobs.sql`.
- Added migration `migrations/0003_media_retention.sql`.
- Added tests for:
  - media persistence
  - media pruning
  - referenced media preservation
  - dispatch release after acknowledgement

## Connector Protocol Changes

- Added shared Go protobuf module under `proto/go`.
- Moved Go connector protobuf imports from Telegram-private path to shared
  `github.com/akshettrj/wuphf/proto/go/connector/v1`.
- Added proto support for:
  - media upload/download
  - `Attachment.media_id`
  - `MediaMetadata`
  - `MediaChunk`
- Completed `ReplyContext` protocol refactor:
  - model has `ReplyContext`
  - proto has `ReplyContext` (legacy `reply_to_platform_message_id` kept for
    simple connectors)
  - router routes rich reply context instead of only native platform message ID
  - storage acknowledgement release sets reply context from the parent message,
    reading the parent inside the acknowledgement transaction
  - gRPC dispatch conversion maps `ReplyContext` and mirrors the legacy native
    reply ID for compatibility
  - Rust tests updated for the new dispatch shape
  - Go bindings regenerated under `proto/go`
  - Telegram sender reads the reply target from `ReplyContext`
  - WhatsApp sender builds real quoted replies (stanza ID, own-account
    participant, quoted body)

## Telegram Connector Changes

- Added Go Telegram connector using `gotd/td`.
- Added Telegram config loader and validation.
- Added target registry for endpoint to Telegram peer/topic mapping.
- Added exact incoming peer matching.
- Added topic/thread support via Telegram topic root message ID.
- Added inbound text normalization.
- Added inbound image/file media normalization.
- Added outbound text dispatch.
- Added outbound image/file dispatch.
- Added Telegram reply dispatch using native Telegram message ID.
- Added sent message acknowledgement.
- Added connector registration against Rust core.
- Added shared connector input stream for future event types.
- Added media broker client for upload/download.
- Added tests for:
  - config ambiguity
  - target registry
  - inbound normalization
  - media metadata upload path
  - sent message ID extraction

## WhatsApp Connector Changes

- Added Go WhatsApp connector using `whatsmeow`.
- Added QR linked-device login flow.
- Added terminal QR rendering through `qrterminal`.
- Added connector-private SQLite session database.
- Added WhatsApp config loader and validation.
- Added endpoint to chat JID target registry.
- Added inbound text normalization.
- Added inbound image/file normalization.
- Added media upload through core broker.
- Added outbound text dispatch.
- Added outbound image/file dispatch.
- Added media download from core broker.
- Added group participant normalization.
- Kept linked-account authored messages flowing into core so Rust core decides
  bridge echo suppression.
- Inbound WhatsApp replies are normalized by stanza ID.
- Outbound WhatsApp quoted replies are currently deferred because correct
  WhatsApp reply sending needs quote metadata beyond native platform message
  ID.
- Added tests for:
  - config ambiguity
  - target registry
  - inbound text and reply normalization
  - self-message pass-through for core echo decision
  - inbound media upload
  - outbound file dispatch
  - explicit rejection of unsupported outbound quoted reply

## Repository / Tooling Changes

- Added Go to Nix development shell.
- Added `protoc-gen-go` and `protoc-gen-go-grpc` to Nix development shell.
- Added `*.session.json` to `.gitignore`.
- Added Telegram and WhatsApp connector README files.
- Updated top-level README with connector run/test commands.
- Updated connector protocol documentation.
- Updated architecture documentation.

## Last Known Verified Checks

Before the current incomplete `ReplyContext` refactor, these checks passed:

- `cargo fmt --all -- --check`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `go test ./...` for `proto/go`
- `go vet ./...` and `go test ./...` for Telegram connector
- `go vet ./...` and `go test ./...` for WhatsApp connector
- `git diff --check`
- `nix flake check --all-systems`

## Current Risk

Workspace is green under static checks and unit tests. Live connector behavior
(real Telegram/WhatsApp sessions) is still unverified. The WhatsApp quoted
reply assumes the quoted copy is bridge-authored (true because the router never
dispatches to the origin endpoint) and quotes the parent body as plain text;
non-text quoted parents are not yet reflected in the quoted preview.

## Next Steps

1. Connector reconnect/backoff.
2. Transient dispatch retry behavior.
3. Video/audio/sticker media kinds.
4. Live WhatsApp QR/session smoke test, including quoted-reply rendering.
5. Live Telegram session smoke test.
6. Implement remaining normalized operations: edits, deletes, reactions.
