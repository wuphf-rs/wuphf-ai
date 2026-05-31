# Implementation Status

This document records current workspace changes and next steps.

## Current State

The repository now contains a Rust core runtime plus Go Telegram and WhatsApp
connector work. Routing has moved from static endpoint/group configuration to
rule-based dynamic routing (`dynamic-routing-design.md`): the core resolves raw
platform addresses to endpoints through an alias table, lazily mints
conversations/groups, and provisions mirror threads on demand. Connectors are
now raw pipes that own no mapping. The Rust workspace and both Go connectors
build and pass their unit tests.

## Dynamic Routing Migration

- Added `Rule`/`RuleSide`/`SideKind` (`single`/`source`/`mirror`) model with
  explicit include/exclude ID filters; rules replace `[[endpoints]]` and
  `[[groups]]`.
- Added `Alias`, `Conversation`, and `ThreadMapping` model types plus SQLite
  tables and lazy resolution, so a raw `SourceAddress` resolves (or mints) an
  endpoint/conversation/group, and mirror threads are recorded once and reused.
- Added `ConnectorCapabilities` (`supports_threads`, `supports_topic_create`);
  a `mirror` side requires both.
- Added identity-migration handling: a new platform address links to an
  existing endpoint and becomes the canonical alias, keeping both addresses
  resolvable (drives WhatsApp phone-to-LID migration).
- Reworked the wire contract to raw addresses: inbound `MessageCreateEvent`
  carries `chat_id`/`thread_id`/`chat_title`; outbound `MessageCreateDispatch`
  carries `target_chat_id`/`target_thread_id`/`provision_thread`; ack carries an
  optional provisioned `thread_id`; added the `IdentityMigrated` client frame.
- CLI config replaced with `[[connectors]]` (id, platform, capabilities) and
  `[[rules]]` (`[[rules.sides]]` with `kind`); `config/example.toml` updated.
- Both connectors rewritten as raw pipes: Telegram emits typed chat IDs
  (`telegram:channel|chat|user:N`) with an access-hash peer cache and
  provisions forum topics on demand; WhatsApp emits JID chat IDs and reports
  phone-to-LID migrations. Connector configs/READMEs updated.

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
- Added typed chat IDs and an access-hash peer cache populated from update
  entities (no preconfigured peer mapping).
- Added on-demand forum topic provisioning for threaded mirrors.
- Added topic/thread support via Telegram topic root message ID.
- Added inbound text normalization.
- Added inbound image/file media normalization.
- Added outbound text dispatch.
- Added outbound image/file dispatch.
- Added inbound video/audio/sticker normalization (documents classified by
  Telegram document attributes).
- Added outbound video/audio/sticker dispatch.
- Added Telegram reply dispatch using native Telegram message ID.
- Added sent message acknowledgement.
- Added connector registration against Rust core.
- Added connector reconnect with exponential backoff.
- Added bounded transient dispatch retry.
- Added shared connector input stream for future event types.
- Added media broker client for upload/download.
- Added tests for:
  - config validation (required fields, topic-create needs threads)
  - typed peer formatting and access-hash peer cache resolution
  - inbound normalization (raw address, topic thread, logical reply, title)
  - media metadata upload path
  - sent message ID extraction

## WhatsApp Connector Changes

- Added Go WhatsApp connector using `whatsmeow`.
- Added QR linked-device login flow.
- Added terminal QR rendering through `qrterminal`.
- Added connector-private SQLite session database.
- Added WhatsApp config loader and validation.
- Emits/executes messages keyed by raw WhatsApp JID chat IDs (no mapping).
- Added phone-to-LID identity migration reporting.
- Added inbound text normalization.
- Added inbound image/file/video/audio/sticker normalization.
- Added media upload through core broker.
- Added outbound text dispatch.
- Added outbound image/file/video/audio/sticker dispatch.
- Added media download from core broker.
- Added group participant normalization.
- Added connector reconnect with exponential backoff.
- Added bounded transient dispatch retry.
- Kept linked-account authored messages flowing into core so Rust core decides
  bridge echo suppression.
- Inbound WhatsApp replies are normalized by stanza ID.
- Outbound WhatsApp quoted replies build real quote metadata (stanza ID,
  own-account participant, quoted body). Non-text parents are quoted as a typed
  stub (kind plus caption) so clients render a media reply.
- Added tests for:
  - config validation (required fields, topic-create needs threads)
  - inbound text and reply normalization with raw JID chat IDs
  - phone-to-LID identity migration derivation
  - self-message pass-through for core echo decision
  - inbound media upload
  - outbound file dispatch
  - outbound quoted reply context (text and media parents)
  - rejection of a legacy reply lacking quote context

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
dispatches to the origin endpoint) and renders non-text parents as a typed stub
rather than the original media preview.

## Next Steps

1. Retitle a provisioned mirror thread when its source conversation is renamed
   (`RetitleThread` dispatch + chat-metadata-changed event), deferred from the
   dynamic-routing work.
2. Live WhatsApp QR/session smoke test, including phone-to-LID migration,
   quoted-reply rendering, and video/audio/sticker round-trips.
3. Live Telegram session smoke test, including forum topic provisioning and
   video/audio/sticker round-trips.
4. Symmetric N-to-N rules (every side both source and mirror).
5. Implement remaining normalized operations: edits, deletes, reactions.
