# Plan 054: whatsapp-rust — outbound media of every kind, rendering options, and the SDK media download

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
> **Drift check (run first)**: plans that must be DONE: **051** (`plans/README.md`
> row 051 DONE). 053 is a soft dependency (see "Interaction with 053").
> Find 051's last commit with `git log --oneline --grep='mark plan 051 done' -1`
> (call it `$P049`), then run
> `git log --oneline $P049..HEAD -- crates/wuphf-connector crates/wuphf-proto crates/wuphf-runtime/src/media.rs connectors/whatsapp-rust connectors/whatsapp/internal/whatsapp/sender.go`.
> Expected: no commits, or only commits of plans 052, 053, 055 and 056 that
> `plans/README.md` marks DONE. What must still hold (if any is false, STOP;
> a fact a DONE plan changed as its own plan says counts as holding):
> - `crates/wuphf-connector/src/media.rs`: `MediaClient` has `connect` and
>   `upload` only (upload at 166-188; the bearer token is added per request
>   from `self.authorization`). `crates/wuphf-proto/media.proto:9-12`
>   `MediaTransport { UploadMedia; DownloadMedia(DownloadMediaRequest {
>   media_id }) returns (stream MediaChunk { chunk }) }`. The core's service
>   (`crates/wuphf-runtime/src/media.rs:115-140`) answers an unknown id with
>   `NOT_FOUND`, a non-UUID with `INVALID_ARGUMENT`, and streams 64 KiB chunks.
> - `connectors/whatsapp-rust/src/sender.rs`: `send_message` dead-letters any
>   dispatch with attachments unless it is a system note, whose attachments
>   are dropped (originally 47-51); replies use `wa::Message::text_with_context`
>   with the reply text as the quoted body; the timer is stamped with
>   `disappearing::stamp` after the reply context. It has **no tests**.
> - `connectors/whatsapp-rust/src/config.rs`: no `[rendering]` section.
> - 051 is in: `connectors/whatsapp-rust/src/media.rs` (`inbound_media`,
>   `MediaFetch`, `MediaSink`) exists; `main.rs` builds one `MediaClient` and
>   hands clones to the avatars and the normalizer.
> - whatsapp-rust library (`~/.cargo/git/checkouts/whatsapp-rust-6e26428647c827f3/5ca80c5`):
>   `src/upload.rs:341` `Client::upload(data: Vec<u8>, MediaType, UploadOptions)
>   -> Result<UploadResponse>`; `:265-279` `UploadResponse` is
>   `#[non_exhaustive]` with public fields and **no public constructor**;
>   `:301-330` `UploadOptions::new()`. `src/media.rs:67` `image_message`,
>   `:92` `video_message` (`gif_playback`), `:119` `document_message`, `:150`
>   `audio_message`, with `ImageOptions` / `VideoOptions` / `DocumentOptions`
>   / `AudioOptions` (20-64; each defaults a missing mime type: `image/jpeg`,
>   `video/mp4`, `application/octet-stream`, `audio/ogg; codecs=opus`).
>   `wacore/src/proto_helpers.rs:423` `MessageExt::set_context_info`
>   (first supported body; promotes a bare `conversation`), `:695`
>   `build_quote_context`.
> - Go WhatsApp (the reference, `connectors/whatsapp/internal/whatsapp/sender.go`):
>   `SendMessage` 98-141, `sendAttachments` 155-185, `attachmentGroupError`
>   191-199, `quotedBody` 301-337, `withContext` 339-361, `attachmentMessage`
>   363-435 (`asFile` 377, `GifPlayback` 408), `stickerMessage` 444-474,
>   `documentMessage` 489-509, `stickerFileName` 528-533,
>   `textExceedsLimit` 541-546, `textFileMessage` 550-565;
>   `connectors/whatsapp/internal/config/config.go:62-71` `RenderingOptions`;
>   `connectors/shared/coreclient/media.go:63-79` `Download`.

## Status

- **Priority**: P1 — anything with media sent from Telegram into WhatsApp
  through the Rust connector is dead-lettered
- **Effort**: L
- **Risk**: MED — the whole outbound path is restructured behind a seam;
  every existing send goes through it
- **Depends on**: 051 (053 soft)
- **Category**: feature / parity
- **Planned at**: commit `313038d`, 2026-10-31

## Why this matters

The Rust connector refuses every dispatch with an attachment
(`Failure::permanent("attachments are not yet supported …")`), so a photo,
a voice note or a file the owner sends from Telegram never reaches WhatsApp,
and the core's info cards and change notes lose their photo. The connector's
rendering policy (`[rendering]`: every attachment as a file; long text as a
`.txt`) does not exist either. The sender also has no tests at all, so
none of this can be changed safely today.

Plan 4 of 6 (051–056) bringing the Rust connector to parity with the Go one.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The Rust SDK gains `MediaClient::download`, mirroring Go `coreclient.MediaClient.Download`, with its own tests. |
| 2 | Outbound media of every kind uses the library builders `image_message`, `video_message` (`gif_playback` from the attachment's animated flag), `document_message`, `audio_message`, with `Client::upload`. |
| 3 | The dispatch text is the caption (image, video, document); audio carries none, as in Go. |
| 4 | Several attachments go out as consecutive messages: the text as the first one's caption, the reply context on the first only, the first message's id returned as the copy; a failure after the first was sent is **permanent** (no duplicates on redelivery), a failure on the first keeps its own retry semantics — exactly Go `sender.go:155-199`. |
| 5 | `[rendering] attachments_as_files` (every attachment as a document) and `max_message_length` (longer text, in Unicode scalar values, goes out as a `message.txt` document; 0 = unbounded), like Go. |
| 6 | A quoted media parent is a typed stub (Go `quotedBody`): kind, mime type, caption, a document's file name — not empty text. |
| 7 | Every part of a content send carries the disappearing timer (045's rule); edits and deletes carry none. |
| 8 | 054 adds a sender test harness: Rust `sender.rs` has none today. Coverage comparable to Go's `sender_test.go` (25 tests). |

### Planner decisions (within the settled scope)

- **The seam sits above the library builders** (owner decision D2 below): a
  `WaClient` trait whose `upload` takes the bytes plus an `OutboundMedia`
  (`Image(ImageOptions)`, `Video(VideoOptions)`, `Audio(AudioOptions)`,
  `Document(DocumentOptions)`) and returns the built `wa::Message`. The live
  implementation calls `Client::upload` and the matching builder; the test
  fake records the request and returns a message carrying the same options.
  Everything the connector decides — kind, options, caption, context, timer,
  part order, failure class — is tested; the live adapter is a few lines of
  glue over library calls.
- **Stickers are sent as documents until plan 055**, the way Go falls back
  when it cannot convert a sticker (`stickerFileName`, no caption). That
  never loses content; 055 replaces it with real stickers.
- **System notes are sent like any content** — text and attachments, never a
  quote (Go `sender.go:104-112`; 046 sent only their text from this
  connector because it had no media).
- **A download from the core that fails is transient** (the core
  redelivers), as in Go; an unknown attachment kind is permanent.
- **Rendering options are read from the file in this plan** (a `Runtime`
  struct flattened into `Config`, the shape of Go's `config.Runtime`); plan
  056 makes them live settings.

## Owner decisions (answered 2026-10-31)

| # | Library gap | Options | Recommendation | Owner answer |
|---|---|---|---|---|
| D2 | `UploadResponse` is `#[non_exhaustive]` with no public constructor (`src/upload.rs:265-279`), and the library builders take one by value. A test outside the crate can neither fake an upload result nor call a builder on one. | (a) Seam above the builders: `WaClient::upload(bytes, OutboundMedia) -> wa::Message`; the live adapter is `Client::upload` + the builder; tests assert the options and the rest of the send. (b) Copy `UploadResponse`'s fields into a connector struct and hand-fill every media proto (tests can then build it), dropping the library builders. (c) Upstream a constructor (or `Default`) for `UploadResponse` and wait for a whatsapp-rust release. | (a): keeps the settled "use the library builders", needs nothing upstream, and leaves only untestable-by-construction glue untested. (b) duplicates the builders; (c) blocks the plan on upstream. | **(a)** |
| D3 | The library builders always set a mime type: a missing one becomes `image/jpeg`, `video/mp4`, `application/octet-stream` or `audio/ogg; codecs=opus` (`src/media.rs:77, 103, 129-131, 161-163`). Go sends no mime type when the attachment has none. | (a) Accept the builders' defaults (WhatsApp clients need a mime type to render media anyway). (b) Clear the mime type after building when the attachment had none, to match Go on the wire. | (a): a present, plausible mime type is what WhatsApp's own clients send; the difference shows only for attachments the source platform sent without one. | **(a)**; the attachment's own mime type is always passed when it has one, so the default applies only when it has none. |

D2 and D3 are both answered (a); Step 2 proceeds with the seam above the
builders and the builders' default mime types.

## Interaction with 053

053 (canonical ids, the addressing book, the quote author) is independent but
edits `sender.rs` too. Whichever lands second keeps the other's behaviour:

- If 053 is DONE before this plan: `WhatsAppSender` already holds
  `identity` and `addresses` and has `target()` and `quoted_author()`.
  `WaClient` then also requires 053's `Account + LidStore` (as supertraits);
  `LiveClient` delegates them to its `Client`; the fake implements them
  (reuse 053's `testing::FakeAccount` inside the fake client). The quote
  context keeps 053's participant and gains this plan's typed body.
- If 053 is not DONE: keep the current own-phone participant; 053 will add
  its parts onto this harness (its plan says so in its interaction note — if
  it does not, note it in 053's row when you mark this one done).

## Current state

See the drift check. Also:

- `TimerFallback::resolve(&self, client: &Client, chat_id, chat, dispatched)`
  (`disappearing.rs:237-282`) looks a group up through
  `GroupDirectory::facts(client, jid)` (`groups.rs:126`); it needs a group
  lookup, which the fake must provide.
- `WhatsAppSender::new(handle.client(), fallback)` in `main.rs:181`.
- Go's sender tests (`sender_test.go`): `TestSendFileDispatch`,
  `TestSendAttachmentsAsFilesForcesDocument`, `TestSendOverLengthTextAsFile`,
  `TestSendWithinLengthLimitStaysInline`, `TestTextExceedsLimit`,
  `TestSendOutboundReplyBuildsQuotedContext`,
  `TestSendOutboundReplyQuotesMediaParent`, `TestEditMessageRevisesTargetInPlace`,
  `TestSendRejectsLegacyReplyWithoutContext`,
  `TestSendMultipleAttachmentsCaptionsOnlyTheFirst`,
  `TestSendMultipleAttachmentsDeadLettersAfterPartialDelivery`,
  `TestSendFirstAttachmentFailureStaysRetryable`, the timer tests
  (`TestSendStampsTheTimerOnText`, `…OnEveryAttachmentKind`,
  `…EveryPartOfAMultiAttachmentSend`, `TestSendMergesTheTimerIntoAReplyContext`,
  `TestSendWithAnOffOrMissingTimerSetsNoExpiration`,
  `TestEditAndDeleteCarryNoTimer`), `TestSendRendersASystemNote`, and the
  sticker tests (055).

## Commands you will need

```sh
pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv   # must print nothing

export CARGO_BUILD_JOBS=2
nice -n 19 cargo test -p wuphf-connector
nice -n 19 cargo clippy -p wuphf-connector --all-targets -- -D warnings
cargo fmt --check

RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo test --manifest-path connectors/whatsapp-rust/Cargo.toml
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --manifest-path connectors/whatsapp-rust/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path connectors/whatsapp-rust/Cargo.toml --check
```

## Scope

In: `crates/wuphf-connector/{src/media.rs,tests/media.rs}`,
`connectors/whatsapp-rust/{config.example.toml,src/main.rs,src/config.rs,src/sender.rs,src/outbound.rs (new),src/wa_client.rs (new),src/disappearing.rs}`,
`README.md` (Rust connector section), `plans/README.md`.

Out: stickers (055: conversion, EXIF, `StickerMessage`), live settings (056),
albums and anything neither connector has, the core, the proto, the Go
connector, `.env.local`, `config.local.toml`, `*.session.json`, `.claude/`,
`connectors/instagram/**`, `plans/048-*`.

## Git workflow

Commit directly on `v2`, one commit per step. Do not push. No branches.

## Steps

### Step 0: Baseline

Drift check. `nice -n 19 cargo test -p wuphf-connector` → record the count.
Alone: whatsapp-rust tests → record the count.

### Step 1: SDK media download

`crates/wuphf-connector/src/media.rs`:

```rust
/// Download the stored media `media_id` (Go `MediaClient.Download`): the
/// core streams it in chunks, which are joined in order.
pub async fn download(&self, media_id: &str) -> Result<Vec<u8>, SessionError> {
    let mut request = tonic::Request::new(pb::DownloadMediaRequest { media_id: media_id.to_owned() });
    request.metadata_mut().insert("authorization", self.authorization.clone());
    let mut stream = self.client.clone().download_media(request).await?.into_inner();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.message().await? {
        bytes.extend_from_slice(&chunk.chunk);
    }
    Ok(bytes)
}
```

Update the module doc ("Uploads to and downloads from the core's media
store").

Tests (`crates/wuphf-connector/tests/media.rs`, its existing `serve`
harness):

1. `a_download_returns_every_chunk` — upload 150 000 bytes (more than two
   64 KiB chunks, not a multiple), download by the returned id → equal
   bytes.
2. `a_download_with_the_wrong_token_is_refused` → `Unauthenticated`.
3. `an_unknown_media_id_is_not_found` — a fresh UUID → `NotFound`; `"x"` →
   `InvalidArgument`.

**Verify**: `nice -n 19 cargo test -p wuphf-connector` → Step 0 count + 3;
clippy clean. Mutation: drop the authorization header → test 1 fails.
Restore.
Commit: `feat(connector-sdk): download media from the core`.

### Step 2: The sender behind a client port, with a test harness

D2 and D3 are answered (a).

1. `config.rs` (Go `config.Runtime` / `RenderingOptions`):
   ```rust
   /// Settings that may change while the connector runs (plan 056 makes them
   /// live). Flattened, so the file keeps its layout (`[rendering]`).
   #[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
   pub struct Runtime {
       #[serde(default)]
       pub rendering: Rendering,
   }
   /// How mirrored copies are rendered on WhatsApp: this connector's own
   /// policy; the core dispatches content only.
   #[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
   pub struct Rendering {
       /// Send every attachment as a file, whatever its kind.
       #[serde(default)]
       pub attachments_as_files: bool,
       /// Longer text is sent as a plain-text file; 0 means no limit.
       #[serde(default)]
       pub max_message_length: u32,
   }
   // in Config:
   #[serde(flatten)]
   pub runtime: Runtime,
   ```
   (`serde::Serialize` is for 056's declaration; the connector already
   depends on `serde` with `derive`.)
2. New `src/wa_client.rs`:
   ```rust
   //! The sender's view of WhatsApp: sending, editing, revoking, uploading
   //! media, and a group's facts. The live implementation wraps the
   //! whatsapp-rust client; tests replace it.

   /// What a media upload becomes: the library builder that carries it, with
   /// its options.
   pub enum OutboundMedia {
       Image(ImageOptions),
       Video(VideoOptions),
       Audio(AudioOptions),
       Document(DocumentOptions),
   }

   pub trait WaClient: Send + Sync + 'static {
       fn send(&self, to: Jid, message: wa::Message) -> impl Future<Output = anyhow::Result<String>> + Send;
       fn edit(&self, to: Jid, id: String, message: wa::Message) -> impl Future<Output = anyhow::Result<String>> + Send;
       fn revoke(&self, to: Jid, id: String) -> impl Future<Output = anyhow::Result<()>> + Send;
       /// Encrypt and upload `bytes` to WhatsApp and build the message that
       /// carries them.
       fn upload(&self, bytes: Vec<u8>, media: OutboundMedia) -> impl Future<Output = anyhow::Result<wa::Message>> + Send;
       /// A group's facts (subject, timer, …), cached.
       fn group_facts(&self, group: &Jid) -> impl Future<Output = Option<GroupFacts>> + Send;
   }

   /// The live client: `Client::upload` + the library builder per kind
   /// (`MediaType::Image` / `Video` / `Audio` / `Document`,
   /// `UploadOptions::new()`), the client's send / edit / revoke, and the
   /// shared group directory.
   pub struct LiveClient { client: Arc<Client>, groups: Arc<GroupDirectory> }
   ```
   (If 053 is DONE, add its traits as described under "Interaction with
   053".)
3. `TimerFallback::resolve` takes `wa: &impl WaClient` instead of `&Client`
   and asks `wa.group_facts(chat)`; its `groups` field goes (the live client
   owns the directory).
4. `sender.rs`: `WhatsAppSender<C: WaClient, M: CoreMedia>` with `client: C`,
   `media: M`, `runtime: Runtime`, `fallback: Option<TimerFallback>` (+ 053's
   fields). `CoreMedia` is a one-method trait (`download(&self, media_id:
   &str) -> Result<Vec<u8>, SessionError>`) implemented for
   `wuphf_connector::MediaClient` (Go's `MediaBroker.Download`). Port the
   current behaviour onto it unchanged (text, reply, note, edit, delete,
   timer); attachments are still refused in this step. `main.rs` builds
   `LiveClient { client: handle.client(), groups }`, passes a `MediaClient`
   clone and `config.runtime.clone()`.
5. Tests: a `#[cfg(test)] mod tests` in `sender.rs` with
   `FakeClient { sends: Mutex<Vec<(Jid, wa::Message)>>, uploads:
   Mutex<Vec<(Vec<u8>, OutboundMedia)>>, fail_send_at: Option<usize>, groups:
   HashMap<String, GroupFacts>, .. }` — its `upload` returns a message whose
   body matches the request's kind and carries its caption, mime type, GIF
   flag and file name, so the sender's context and timer land on a real
   body — and `FakeMedia { blobs: HashMap<String, Vec<u8>>, fail: bool }`.
   Port-the-behaviour tests:
   1. `text_is_sent_as_text`.
   2. `a_reply_quotes_its_parent` — `stanza_id`, participant, quoted text.
   3. `a_reply_id_without_a_context_is_permanent` (Go
      `TestSendRejectsLegacyReplyWithoutContext`).
   4. `a_note_never_quotes` (Go `TestSendRendersASystemNote`, text part).
   5. `the_timer_is_stamped_on_text` — bare text → extended text with
      `expiration` and `ephemeral_setting_timestamp` (Go
      `TestSendStampsTheTimerOnText`).
   6. `an_off_or_missing_timer_sets_nothing`.
   7. `edits_and_deletes_return_the_target_id_and_carry_no_timer` (Go
      `TestEditMessageRevisesTargetInPlace`, `TestEditAndDeleteCarryNoTimer`).
   8. `a_group_without_a_dispatched_timer_uses_its_own` — fallback on, a group
      with 604800 in the fake → stamped, and a chat-update frame queued.

**Verify**: whatsapp-rust tests → Step 0 count + 8 (+ config tests if you
add them), green; clippy clean. The live behaviour is unchanged (no
attachments yet).
Commit: `refactor(whatsapp-rust): put the sender behind a client port, with tests`.

### Step 3: Single attachments, rendering options and media quotes

New `src/outbound.rs` (pure where it can be):

```rust
/// The upload request for one attachment (Go `attachmentMessage`): its kind,
/// or a document when `as_file`; the caption on image, video and document.
/// A sticker is a document until plan 055 (`sticker_file_name`, no caption).
/// An unspecified kind is a permanent failure.
pub fn outbound_media(attachment: &pb::Attachment, caption: &str, as_file: bool)
    -> Result<OutboundMedia, Failure>;

/// Whether `text` is longer than `limit` Unicode scalar values (0 = unbounded).
pub fn text_exceeds_limit(text: &str, limit: u32) -> bool;

/// Over-length text as a plain-text document (`message.txt`, `text/plain`).
pub fn text_file_media() -> OutboundMedia;

/// The quoted parent for WhatsApp's preview (Go `quotedBody`): plain text, or
/// a typed stub of the first attachment (image / video with caption and mime
/// type, document with caption, mime type and file name, audio and sticker
/// with mime type), which clients render as a media reply.
pub fn quoted_body(reply: &pb::ReplyContext) -> wa::Message;

/// A sticker's fallback document name: its own, else "sticker" + the source
/// extension (`.webm`, `.tgs`, the file name's, else `.bin`).
pub fn sticker_file_name(attachment: &pb::Attachment) -> String;
```

Empty strings become `None` in the options (Go `optionalProtoString`); a
video's `gif_playback` is `Some(attachment.animated)`; audio gets no caption
and no `ptt`.

`sender.rs` `send_message` (one attachment, or none):

1. Pin `let runtime = self.runtime.clone();` (056 turns this into a live
   read).
2. Reply context unless a system note (unchanged rules).
3. Timer (`fallback` or the dispatch's).
4. Body: one attachment → download it from the core (`self.media.download`),
   `outbound_media(..., runtime.rendering.attachments_as_files)`, then
   `self.client.upload(bytes, media)`; no attachment and
   `text_exceeds_limit` → upload the text bytes as `text_file_media()`;
   otherwise `wa::Message::text(text)`.
5. `message.set_context_info(context)` when there is one, then
   `stamp(&mut message, timer)`, then send.

The quoted body becomes `quoted_body(reply)` (the participant rule is
unchanged by this plan; see "Interaction with 053").

Tests (`outbound.rs` and `sender.rs`):

1. `each_kind_uploads_with_its_options` — image (caption, mime), video
   (caption, `gif_playback` true / false), audio (mime only), document
   (caption, file name) (Go `TestSendFileDispatch` and the kind cases).
2. `attachments_as_files_forces_documents` — every kind → `Document`, file
   name kept, caption kept (Go `TestSendAttachmentsAsFilesForcesDocument`).
3. `over_length_text_goes_out_as_a_file` — limit 5, text of 6 scalar values
   (use a multi-byte character, e.g. `"héllo!"`) → one `Document` upload of
   the UTF-8 bytes named `message.txt`, `text/plain`; the reply context is on
   the document (Go `TestSendOverLengthTextAsFile`).
4. `text_within_the_limit_stays_inline` and `text_exceeds_limit_counts_characters`
   (0 = unbounded; exactly the limit → inline) (Go `TestTextExceedsLimit`,
   `TestSendWithinLengthLimitStaysInline`).
5. `a_media_parent_is_quoted_as_a_typed_stub` — table over the five kinds
   and an attachment-less parent (Go `TestSendOutboundReplyQuotesMediaParent`).
6. `the_reply_context_lands_on_the_media` — image with a reply → the image's
   `context_info` has the stanza id and the timer (merged, not replaced)
   (Go `TestSendMergesTheTimerIntoAReplyContext`).
7. `a_sticker_goes_out_as_a_document_for_now` — `KIND_STICKER`, mime
   `video/webm`, no file name → `Document` named `sticker.webm`, no caption.
8. `an_unspecified_kind_is_permanent`.
9. `a_failed_core_download_is_transient`.
10. `a_note_sends_its_photo` — system note with one image → an image upload
    with the note text as caption, no quote.

**Verify**: whatsapp-rust tests → + 10 (or more, table cases inside), green.
Mutation: pass `as_file = false` always → test 2 fails; count bytes instead
of characters → test 3 fails. Restore.
Commit: `feat(whatsapp-rust): send photos, videos, audio and files, with the rendering options`.

### Step 4: Several attachments

`sender.rs`: with more than one attachment, `send_attachments` (Go
`sendAttachments`): for each attachment in order — caption = the dispatch
text on the first only; build (download + `outbound_media` + upload); the
reply context on the first only; the timer on **every** part; send. Return
the first part's id. Wrap any failure at index `i > 0` (building or sending)
as

```rust
Failure::permanent(format!(
    "attachment {} of a multi-attachment message failed after {} were sent: {error}",
    i + 1, i,
))
```

and leave a failure at index 0 as it is (Go `attachmentGroupError`).

Tests:

1. `several_attachments_caption_only_the_first` — three images → three
   sends, caption on the first, context on the first, the returned id is the
   first send's (Go `TestSendMultipleAttachmentsCaptionsOnlyTheFirst`).
2. `a_failure_after_the_first_part_is_permanent` — the fake fails the second
   send → `Failure::Permanent` naming "attachment 2 … after 1 were sent"
   (Go `TestSendMultipleAttachmentsDeadLettersAfterPartialDelivery`).
3. `a_failure_on_the_first_part_stays_retryable` — first send fails →
   transient (Go `TestSendFirstAttachmentFailureStaysRetryable`).
4. `every_part_carries_the_timer` — three parts, each with `expiration`;
   the reply context only on the first (Go
   `TestSendStampsEveryPartOfAMultiAttachmentSend`).
5. `the_timer_is_stamped_on_every_kind` — image, video, audio, document,
   sticker-as-document and the over-length text file (Go
   `TestSendStampsTheTimerOnEveryAttachmentKind`).

**Verify**: whatsapp-rust tests → + 5, green. Mutations: stamp only the
first part → test 4 fails; return the last part's id → test 1 fails; make
every failure permanent → test 3 fails. Restore.
Commit: `feat(whatsapp-rust): send several attachments as consecutive messages`.

### Step 5: Docs and the example config

- `connectors/whatsapp-rust/config.example.toml`: a `[rendering]` block
  copied from Go's (`connectors/whatsapp/config.example.toml`, the
  `[rendering]` paragraph): `attachments_as_files = false`,
  `# max_message_length = 4096`, with the same comments.
- `README.md`, Rust connector section: outbound media of every kind (stickers
  as files until 055), several attachments as consecutive messages, the
  `[rendering]` options.

**Verify**: `grep -n 'attachments_as_files' connectors/whatsapp-rust/config.example.toml README.md`
→ hits.
Commit: `docs(whatsapp-rust): outbound media and rendering options`.

### Step 6: Gate, dev binary, index, and the owner's live test

1. Gate: `cargo test -p wuphf-connector`, clippy, `cargo fmt --check`; then
   alone the whatsapp-rust test, clippy, fmt. Lock files:
   `git diff --stat 313038d -- Cargo.lock connectors/whatsapp-rust/Cargo.lock`
   → empty (no new dependency).
2. Dev binary (check `pgrep` first):
   ```sh
   RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo build --release --manifest-path connectors/whatsapp-rust/Cargo.toml
   install -m 0755 connectors/whatsapp-rust/target/release/wuphf-connector-whatsapp scripts/dev-bin/.bin/wuphf-whatsapp-rust
   ```
3. `plans/README.md`: row 054 DONE with notes (counts, mutations, D2 / D3
   answers, the sticker interim). Commit: `docs(plans): mark plan 054 done`.

**For the owner:**

- **Restart**: the whatsapp-rust connector only (`wwr`).
- **Config** (the connector's own file): optional `[rendering]`
  `attachments_as_files` (default false) and `max_message_length` (default
  0, unbounded).
- **Live test** (from Telegram into a chat mirrored to this connector):
  1. Send a photo with a caption, a video, a GIF, a voice message, a PDF →
     each arrives on WhatsApp as that kind; the GIF loops; the PDF keeps its
     name; captions show.
  2. Send an album of three photos with a caption → three WhatsApp messages,
     the caption on the first. Reply on WhatsApp to the second and check the
     reply reaches Telegram.
  3. Reply from Telegram to a WhatsApp photo → the WhatsApp quote preview
     shows a photo, not empty text.
  4. In a chat with disappearing messages on, send a photo → it shows no
     "will not disappear" mark.
  5. Set `max_message_length = 50`, restart, send a 60-character message →
     it arrives as `message.txt`. Set `attachments_as_files = true`, restart,
     send a photo → it arrives as a file. Put both back.
  6. Send a sticker → it arrives as a file for now (055 makes it a sticker).

## Test plan

| Test | Where | Proves |
|---|---|---|
| download chunks, wrong token, unknown id | wuphf-connector | decision 1 |
| ported text / reply / note / edit / delete / timer / group fallback | whatsapp-rust sender | the harness keeps today's behaviour (decision 8) |
| kinds and options, as-files, long text, typed quotes, context on media, sticker interim, failure classes, note photo | whatsapp-rust outbound, sender | decisions 2, 3, 5, 6 |
| several attachments, partial failure, first failure, timer on every part and kind | whatsapp-rust sender | decisions 4, 7 |

## Done criteria

- [ ] Every test above passes; every listed mutation fails a test.
- [ ] No lock file changed; clippy and fmt clean; whatsapp-rust built alone.
- [ ] Dev binary installed; row 054 DONE with notes and the owner's steps.

## STOP conditions

- The drift check fails, 051 is not DONE, or a "must still hold" fact is
  false.
- A library builder, `Client::upload`, `UploadOptions::new` or
  `MessageExt::set_context_info` is missing or different at `5ca80c5c`.
- Building needs a dependency change in either lock file.
- Any test that existed at Step 0 starts failing for a reason this plan does
  not explain.

## Maintenance notes

- **Never emit frames from the sender except through 045's
  `emit_without_blocking`.** The SDK session loop that calls the sender is
  the one draining the input channel (045's hazard).
- **The live adapter is the only untested code on this path**: keep it a
  thin mapping from `OutboundMedia` to `MediaType` + builder. If
  whatsapp-rust adds an `UploadResponse` constructor, a test of the adapter
  becomes possible.
- **Builder defaults** (D3): an attachment without a mime type goes out with
  the builder's default; Go sends none.
- **Stickers go out as documents until 055.**
- **Rendering options are read once at startup** until 056 makes them live;
  restart the connector after changing them.
