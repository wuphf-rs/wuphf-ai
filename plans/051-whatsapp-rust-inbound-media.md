# Plan 051: whatsapp-rust — inbound media, captions, view-once, and the Android client identity

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
> **Drift check (run first)**: plans that must be DONE: none. Run
> `git log --oneline 313038d..HEAD -- crates/wuphf-connector crates/wuphf-proto connectors/whatsapp-rust connectors/whatsapp/internal`.
> Expected: no commits, or only commits of plans 052–056 that
> `plans/README.md` marks DONE (each plan lists its commit messages). Any
> other commit: re-check every fact below. What must still hold (if any is
> false, STOP):
> - `connectors/whatsapp-rust/Cargo.toml` pins whatsapp-rust and
>   whatsapp-rust-sqlite-storage at `rev = "5ca80c5c"`; the checkout is
>   `~/.cargo/git/checkouts/whatsapp-rust-6e26428647c827f3/5ca80c5`.
> - `connectors/whatsapp-rust/src/normalize.rs`: `normalize` reads
>   `message.protocol_message` (line 218) and `message.reaction_message` (269)
>   on the **raw** message; lines 285-296 drop any message without
>   `text_content()` and warn when it has media; the create event sets
>   `attachments: vec![]` (311); `context_info` (372-407) and `has_attachment`
>   (409-415) read the raw message's fields.
> - `connectors/whatsapp-rust/src/disappearing.rs:34-43`: `message_timer`
>   reads the setting timestamp through `context_info(message)`.
> - `connectors/whatsapp-rust/src/main.rs`: the bot is built at 159
>   (`builder.build()`) and spawned at 160 (`bot.spawn()`); no client profile
>   or device props are set anywhere; `MediaClient::connect` is called once
>   (77-80) for the avatars.
> - `connectors/whatsapp-rust/src/config.rs:213-240`: no `client_mode` key.
> - whatsapp-rust library (checkout above):
>   `wacore/src/proto_helpers.rs:224` `get_base_message` unwraps
>   `device_sent_message`, `ephemeral_message`, the three view-once wrappers,
>   `document_with_caption_message` and `edited_message`; `:291`
>   `is_view_once`; `:337` `get_caption` (image, video, document); `:351`
>   `text_content` (already on the base message). `src/download.rs:272`
>   `Client::download(&self, &dyn Downloadable) -> anyhow::Result<Vec<u8>>`;
>   `wacore/src/download.rs:190-208` implement `Downloadable` for
>   `ImageMessage`, `VideoMessage`, `DocumentMessage`, `AudioMessage`,
>   `StickerMessage`. `wacore/src/client_profile.rs:51` `ClientProfile::android(os_version)`,
>   `:64` `smb_android(os_version)` (both `include_web_info: false`).
>   `src/client/iq_ops.rs:186` `Client::set_client_profile` ("In-memory only;
>   call before each `connect()` on a fresh process"); `wacore/src/store/device.rs:240-242`
>   keeps it `#[serde(skip)]`, and `:461-470`/`:488` build both the login and
>   the registration payload from it. `src/bot.rs:522` `Bot::client()`,
>   `:557` `Bot::spawn`, `:1135` `BotBuilder::with_device_props(DevicePropsOverride)`;
>   `wacore/src/store/device.rs:123` `DevicePropsOverride { os, version,
>   platform_type, history_sync_config }` with `with_os` / `with_platform_type`;
>   `waproto/src/whatsapp.proto:2128` `ANDROID_PHONE = 16`.
> - Go WhatsApp (the reference): `connectors/whatsapp/internal/whatsapp/media.go:46-97`
>   `inboundMedia`; `updates.go:425-477` `normalize`, `:507-547`
>   `unwrapViewOnce` / `markViewOnce` / `isViewOnce`, `:549-564`
>   `messageText`; `deviceprops.go:27-36` `useAndroidClientIdentity`;
>   `internal/config/config.go:130-146, 173-177` `client_mode`.

## Status

- **Priority**: P1 — photos, videos, voice notes, documents and stickers sent
  to the owner on WhatsApp never reach Telegram through the Rust connector
- **Effort**: M
- **Risk**: MED — changes what the connector registers as on WhatsApp (needs
  one re-pair), and every inbound message goes through the new unwrap path
- **Depends on**: — (none)
- **Category**: feature / bug
- **Planned at**: commit `313038d`, 2026-10-31

## Why this matters

The Rust connector drops every inbound message that carries media
(`normalize.rs:285-296`), so it is no alternative to the Go connector for any
real chat. It also reads protocol messages, reactions and reply contexts off
the raw message, so a reaction, an edit or a reply that arrives inside an
ephemeral (disappearing-messages) wrapper is lost or loses its reply link: in
a chat with disappearing messages on, almost every message is wrapped. Finally
it registers as a web companion, to which WhatsApp does not reliably deliver
view-once media; the Go connector registers as an Android client for that
reason (the owner's note `whatsapp-android-client-identity`).

This is plan 1 of 6 that bring the Rust connector to parity with the Go one
(051 inbound media, 052 notices and receipts, 053 identity, 054 outbound
media, 055 stickers, 056 runtime settings).

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Both connectors stay in use. Parity means every Go capability with the same wire semantics and the same canonical chat ids. |
| 2 | Inbound media is downloaded with whatsapp-rust `Client::download`, uploaded to the core with the SDK `MediaClient::upload` (plan 046), and described exactly like Go `media.go` `inboundMedia`: kind, mime type, size (the message's `file_length`), file name (documents only), and the animated flag (video `gif_playback`, sticker `is_animated`). |
| 3 | Captions become the message text, as Go's `messageText` reads them (image, video, document). |
| 4 | View-once media is bridged as the ordinary media it wraps, with the text prefixed `👁️ View once` (alone when there is no caption), like Go's `unwrapViewOnce` + `markViewOnce` (three wrapper variants). |
| 5 | `normalize` reads everything off `MessageExt::get_base_message()`, so ephemeral, view-once, device-sent and document-with-caption wrappers are unwrapped for protocol messages, reactions, reply contexts, mentions and timers too. Tests cover wrapped reactions, edits and replies. |
| 6 | A `client_mode` config key (`android`, the default, or `android_business`) selects the client identity, mirroring Go's `deviceprops.go`: `ClientProfile::android` / `smb_android` applied with `Client::set_client_profile` before the bot connects, and `with_device_props` marking the device `ANDROID_PHONE` with OS `Android`. |
| 7 | The owner agreed to re-pair the Rust session once to verify view-once delivery; the live-test script says how. |
| 8 | Sealed edits authored more than 20 minutes after their message are dropped by the library (`src/message/msg_secret.rs:362-375`); accepted, see Maintenance notes. |

### Planner decisions (within the settled scope)

- **A failed download or upload drops the message** with a `warn` log, and
  the connector keeps running. Go returns the error from its event handler,
  which ends the Go connector's run (`connector.go:124-130, 160-161`); on the
  wire both send nothing for that message, and staying up is what plan 052
  asks of this connector anyway. Delivering the caption alone is not done:
  it would bridge a message that silently lost its media.
- **The media fetch is in the normalizer**, an event handler task where
  awaiting is safe. It runs only for a message that becomes a create (never
  for protocol messages, reactions or chat updates).
- **Two small seams keep the media path testable without a network**:
  `MediaFetch` (implemented for `whatsapp_rust::Client`) and `MediaSink`
  (implemented for `wuphf_connector::MediaClient`), mirroring Go's
  `Downloader` and `MediaBroker` interfaces (`media.go:12-19`).
- **View-once detection**: the library's `is_view_once()` (wrapper present, or
  the inline `view_once` flag), counted only when the base message carries an
  image, video or audio — Go marks exactly those three
  (`updates.go:543-547`). A view-once flag on extended text gets no marker,
  as in Go.
- **The client identity lives in a new `device.rs`** (Go: `deviceprops.go`):
  a pure `client_identity(mode) -> (ClientProfile, DevicePropsOverride)`.

## Owner decisions (answered 2026-10-31)

| # | Library gap | Options | Recommendation | Owner answer |
|---|---|---|---|---|
| D1 | whatsapp-rust's Android profile is not byte-for-byte what whatsmeow sends. Go only flips whatsmeow's `BaseClientPayload` platform and drops `WebInfo`, so its user agent still says `Device: "Desktop"`, `OsVersion: "0.1"` (whatsmeow `store/clientpayload.go:109-111`). `ClientProfile::android(os)` says `device: "Smartphone"` and takes the OS version as an argument (`client_profile.rs:51-62`); the library has no "web payload with an Android platform" constructor. | (a) `ClientProfile::android("0.1")` / `smb_android("0.1")` as the library builds them (device "Smartphone"); (b) the same, then set `device = "Desktop"` so every field matches whatsmeow; (c) a realistic Android version string (e.g. `"13"`). | (a) | **(b)**: match whatsmeow on every field, since Go's exact values are the ones known to deliver view-once. |

D1 is answered (b): build the library profile, then set `device = "Desktop"`
so the user agent matches whatsmeow field for field.

## Current state

- **Inbound path.** `main.rs:135-138` registers `on_message`; the library
  fans `Event::Messages` batches out to it as `MessageContext { message:
  Arc<wa::Message>, info, client }` (`src/bot.rs:139-143`). The message is
  the **raw** decrypted message: wrappers are not removed before dispatch
  (the library itself only reads `get_base_message()` for the timer,
  `src/message/dispatch.rs:20`). `Normalizer::handle` (`normalize.rs:56-88`)
  upserts the push name, resolves titles and mentions (`resolve`, 92-124),
  calls the pure `normalize(info, message, &Resolved)` (211-318), adds the
  DM's other form to a chat update (71-73), sends a first-sighting snapshot
  (78-84), then the frame.
- **Text.** `text_content()` already unwraps (proto_helpers.rs:351), so plain
  wrapped text is bridged today; captions are not read at all.
- **Media upload.** `wuphf_connector::MediaClient` (`crates/wuphf-connector/src/media.rs:141-189`)
  has `connect(core_address, token)` and `upload(metadata, bytes) ->
  Result<pb::Attachment, SessionError>`; `main.rs:77-80` builds one for
  `Avatars`. It is cheap to clone.
- **Tests.** 28 whatsapp-rust tests (normalize 6, disappearing 9, chats 5,
  names 4, contacts 3, groups 1); the normalize tests build `MessageInfo`
  with a helper `info(ephemeral_expiration)` (`normalize.rs:428-439`) and
  call `normalize` directly.
- **Go reference.** `MediaCodec.Inbound` (`media.go:30-44`) downloads and
  uploads one attachment; `normalize` (`updates.go:425-477`) drops a message
  whose media could not be described, marks view-once text, renders mentions,
  and drops a message with neither text nor attachments. Go's tests:
  `TestMediaCodecUploadsInboundImage`, `TestUnwrapViewOnceExposesInnerMedia`,
  `TestUnwrapViewOncePassesThroughPlainMessage`, `TestMarkViewOncePrefixesCaption`,
  `TestUseAndroidClientIdentity`.

## Commands you will need

```sh
# before every build or test: must print nothing
pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv

# whatsapp-rust, alone
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo test --manifest-path connectors/whatsapp-rust/Cargo.toml
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --manifest-path connectors/whatsapp-rust/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path connectors/whatsapp-rust/Cargo.toml --check
```

The first whatsapp-rust build of a session is slow; later ones only rebuild
the connector crate.

## Scope

In: `connectors/whatsapp-rust/{config.example.toml,src/main.rs,src/config.rs,src/normalize.rs,src/disappearing.rs,src/media.rs (new),src/device.rs (new)}`,
`README.md` (the Rust connector section), `plans/README.md`.

Out: outbound media (054), stickers outbound (055), receipts and notices
(052), LID canonicalization (053), the Go connector, the core and the proto,
`.env.local`, `config.local.toml`, `*.session.json`, `.claude/`,
`connectors/instagram/**`, `plans/048-*`.

## Git workflow

Commit directly on `v2`, one commit per step, with the message given in the
step. Do not push. Do not create branches.

## Steps

### Step 0: Baseline

Run the drift check. Then run the whatsapp-rust tests once (alone) and record
the count (expected 28 passed at `313038d`).

### Step 1: Read everything off the base message

`connectors/whatsapp-rust/src/normalize.rs`:

- At the top of `normalize`: `let base = message.get_base_message();` and use
  `base` for `protocol_message`, `reaction_message`, `has_attachment` and the
  text. Keep the `message` parameter (the timer and future callers need the
  original).
- `context_info(message)` reads `message.get_base_message()` itself, so every
  caller (reply target, mentions in `resolve`, `message_timer` in
  `disappearing.rs`) sees the unwrapped content:
  ```rust
  /// The reply / mention / timer context of a message's content, read off
  /// the base message so ephemeral, view-once and device-sent wrappers are
  /// looked through.
  pub fn context_info(message: &wa::Message) -> Option<&wa::ContextInfo> {
      let base = message.get_base_message();
      base.extended_text_message.as_option().and_then(|m| m.context_info.as_option())
          .or_else(|| …image, video, document, audio, sticker, as today…)
  }
  ```
- `has_attachment` takes the base message (callers pass `base`).

Tests (`normalize.rs` `mod tests`; build wrapped messages by hand with
`MessageField::some(wa::message::FutureProofMessage { message:
MessageField::some(inner), .. })` for `ephemeral_message`):

1. `a_wrapped_reaction_is_bridged` — an `ephemeral_message` holding a
   `reaction_message` (key id `wa-0`, text `👍`) → a `ReactionEvent` for
   `wa-0` with `👍`.
2. `a_wrapped_edit_is_bridged` — an `ephemeral_message` holding a
   `protocol_message` of type `MESSAGE_EDIT` (key id `wa-0`, edited text
   `new`) → a `MessageEditEvent` for `wa-0` with text `new`.
3. `a_wrapped_reply_keeps_its_parent` — an `ephemeral_message` holding an
   `extended_text_message` with `context_info.stanza_id = "wa-0"` → the
   create's `reply_to_platform_message_id` is `wa-0`.
4. `a_wrapped_revoke_is_bridged` — `ephemeral_message` holding a `REVOKE` →
   `MessageDeleteEvent`.
5. `a_wrapped_message_carries_its_timer_timestamp` — `ephemeral_message`
   holding text with `context_info { expiration: 86400,
   ephemeral_setting_timestamp: 1_700_000_000 }`, `info(Some(86_400))` → the
   create's timer has the setting timestamp (it was `None` before this step).

**Verify**: whatsapp-rust `cargo test` → Step 0 count + 5, green.
Mutation: make `normalize` read `message.reaction_message` instead of
`base.reaction_message` → test 1 fails; make `context_info` read the raw
message → tests 3 and 5 fail. Restore.
Commit: `fix(whatsapp-rust): read reactions, edits, replies and timers through message wrappers`.

### Step 2: Describe and fetch inbound media

New `connectors/whatsapp-rust/src/media.rs` (mirrors Go `media.go`):

```rust
//! Inbound media: what a WhatsApp message's attachment is (described like the
//! Go connector's `inboundMedia`), fetched from WhatsApp and stored in the
//! core's media store so the create event can name it.

use whatsapp_rust::download::Downloadable;

/// The attachment a base message carries, and where to download it from.
pub struct InboundMedia<'a> {
    pub metadata: pb::MediaMetadata,
    pub source: &'a dyn Downloadable,
}

/// Image, then document, then video, then audio, then sticker — Go's order.
/// `size_bytes` is the message's `file_length` (0 when absent, as Go's
/// `GetFileLength`); empty mime types and file names are `None`; only a
/// document carries a file name; `animated` is a video's `gif_playback` or a
/// sticker's `is_animated`.
pub fn inbound_media(base: &wa::Message) -> Option<InboundMedia<'_>>;

/// Downloads WhatsApp media (Go: `Downloader`).
pub trait MediaFetch: Send + Sync {
    fn download(&self, source: &dyn Downloadable)
        -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send;
}
impl MediaFetch for Client { /* self.download(source) */ }

/// Stores bytes in the core's media store (Go: `MediaBroker.Upload`).
pub trait MediaSink: Send + Sync {
    fn upload(&self, metadata: pb::MediaMetadata, bytes: Vec<u8>)
        -> impl Future<Output = Result<pb::Attachment, SessionError>> + Send;
}
impl MediaSink for MediaClient { /* self.upload(metadata, bytes) */ }

#[derive(Debug, thiserror::Error)] // or a plain enum with Display; no new dependency
pub enum InboundMediaError { Download(anyhow::Error), Upload(SessionError) }

/// The message's attachment, downloaded and stored: `Ok(None)` when it has
/// none.
pub async fn fetch(fetch: &impl MediaFetch, sink: &impl MediaSink, base: &wa::Message)
    -> Result<Option<pb::Attachment>, InboundMediaError>;
```

Do not add a dependency for the error type: implement `Display` by hand if
`thiserror` is not already a direct dependency of the connector (it is not;
`anyhow` is).

Tests (`media.rs` `mod tests`):

1. `each_media_kind_is_described_like_go` — table over image (mime
   `image/jpeg`, length 10), document (file name `a.pdf`, mime
   `application/pdf`), video with and without `gif_playback`, audio, sticker
   with and without `is_animated` → kind, mime, size, file name (document
   only), animated.
2. `empty_fields_are_absent` — an image with empty mime → `mime_type: None`,
   `size_bytes: Some(0)` when `file_length` is unset.
3. `text_has_no_media` — `wa::Message::text("hi")` → `None`.
4. `fetch_uploads_what_it_downloads` (`#[tokio::test]`) — a fake `MediaFetch`
   returning `b"jpeg"` and a fake `MediaSink` recording its call and
   returning an attachment with media id `m-1` → `Ok(Some(..))` with `m-1`,
   and the sink saw the image metadata and the bytes.
5. `a_failed_download_uploads_nothing` — the fake fetch errors → `Err(Download)`
   and the sink was never called.

**Verify**: whatsapp-rust `cargo test` → green, + 5.
Commit: `feat(whatsapp-rust): describe and fetch inbound media`.

### Step 3: Bridge media, captions and view-once

`normalize.rs`:

- `Resolved` gains `attachments: Vec<pb::Attachment>`.
- `Normalizer` gains `media: MediaClient` (the one `main.rs` already builds;
  pass a clone). In `handle`, after the push-name upsert and before
  `resolve`: when the base message is not a protocol message or a reaction
  and `inbound_media(base)` is `Some`, call `media::fetch(&*ctx.client,
  &self.media, base)`; on `Err`, `warn!(chat, message_id, %error, "dropping a
  WhatsApp media message: the media could not be fetched")` and return; on
  `Ok(Some(attachment))` put it in `Resolved.attachments`.
- In `normalize`, replace the media drop at 285-296:
  ```rust
  // A message whose media was not fetched is not bridged: delivering its
  // caption alone would silently lose the attachment (Go drops it too).
  if has_attachment(base) && resolved.attachments.is_empty() {
      return None;
  }
  let text = message_text(base);                   // text, else caption
  let text = mark_view_once(message, base, text);  // "👁️ View once"
  let text = render_mentions(&text, &resolved.mentions);
  if text.is_empty() && resolved.attachments.is_empty() {
      return None;
  }
  ```
  and set `attachments: resolved.attachments.clone()` and `text:
  (!text.is_empty()).then_some(text)` on the create (Go leaves `Text` unset
  when empty).
- New pure helpers in `normalize.rs`:
  ```rust
  /// The message's text: its body, else its caption (image, video,
  /// document), as Go's `messageText`.
  fn message_text(base: &wa::Message) -> String;

  /// Marks a view-once photo, video or voice note so the mirror copy is
  /// recognisable as one: the marker alone, or the marker and the caption.
  const VIEW_ONCE_MARKER: &str = "👁️ View once";
  fn mark_view_once(message: &wa::Message, base: &wa::Message, text: String) -> String;
  ```
  `mark_view_once` applies only when `message.is_view_once()` and the base
  has an image, video or audio message.

Tests (`normalize.rs`):

1. `a_photo_with_a_caption_is_bridged` — base image with caption `look`,
   `Resolved.attachments = [m-1]` → create with text `look` and the
   attachment.
2. `a_photo_without_a_caption_is_bridged` — no caption → text `None`, one
   attachment.
3. `media_that_was_not_fetched_is_dropped` — image, no resolved attachment →
   `None`.
4. `a_document_caption_is_read` — document with caption (also wrapped in
   `document_with_caption_message`) → text is the caption.
5. `view_once_media_is_marked` — table over `view_once_message`,
   `view_once_message_v2`, `view_once_message_v2_extension` wrapping an image
   with caption `hi` → text `"👁️ View once\nhi"`; a video without caption →
   `"👁️ View once"`; a voice note (audio with `view_once: true`, no wrapper)
   → `"👁️ View once"`; an ordinary image with caption → caption unchanged.
6. `mentions_render_in_captions` — image caption `hey @1666` with a resolved
   mention → `hey @Bob`.

**Verify**: whatsapp-rust `cargo test` → green, + 6. Mutation: drop the
`has_attachment(base) && …` guard → test 3 fails; mark view-once only for the
first wrapper variant → test 5 fails. Restore.
Commit: `feat(whatsapp-rust): bridge inbound media, captions and view-once messages`.

### Step 4: Register as an Android client

D1 is answered (b): the library profile with `device` forced to `"Desktop"`.

- `config.rs`:
  ```rust
  /// Which WhatsApp client the session registers as. Both pair as an Android
  /// client, so WhatsApp delivers view-once media, which it withholds from a
  /// web companion; `android_business` is for a WhatsApp Business (SMB)
  /// account, which cannot pair as the personal client.
  #[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
  #[serde(rename_all = "snake_case")]
  pub enum ClientMode { #[default] Android, AndroidBusiness }

  #[serde(default)]
  pub client_mode: ClientMode,
  ```
  An unknown value fails `Config::load` with serde's "unknown variant"
  message (Go: `client_mode must be "android" or "android_business"`).
- New `src/device.rs` (Go: `deviceprops.go`):
  ```rust
  //! The client identity the session registers as. WhatsApp does not reliably
  //! deliver view-once media to a web companion, so the session announces
  //! itself as an Android client (the Baileys#2201 approach the Go connector
  //! uses): an Android user-agent platform without web info, and a linked
  //! device marked as an Android phone.

  /// The OS label WhatsApp shows for this linked device.
  pub const ANDROID_DEVICE_OS: &str = "Android";
  /// The user agent's OS version: whatsmeow's, which the Go connector sends.
  const OS_VERSION: &str = "0.1";
  /// The user agent's device: whatsmeow's, which the Go connector sends.
  const USER_AGENT_DEVICE: &str = "Desktop";

  pub fn client_identity(mode: ClientMode) -> (ClientProfile, DevicePropsOverride) {
      let mut profile = match mode {
          ClientMode::Android => ClientProfile::android(OS_VERSION),
          ClientMode::AndroidBusiness => ClientProfile::smb_android(OS_VERSION),
      };
      // whatsmeow only flips the platform of its web payload, so its user
      // agent still says "Desktop"; match it field for field (owner D1 = b).
      profile.device = USER_AGENT_DEVICE.to_string();
      let props = DevicePropsOverride::new()
          .with_os(ANDROID_DEVICE_OS)
          .with_platform_type(wa::device_props::PlatformType::ANDROID_PHONE);
      (profile, props)
  }
  ```
  (Confirm the import paths with grep: `whatsapp_rust::ClientProfile`,
  `whatsapp_rust::wacore::store::DevicePropsOverride`.)
- `main.rs`: before building, `let (profile, device_props) =
  device::client_identity(config.client_mode);` and
  `warn!(client_mode = ?config.client_mode, "registering the WhatsApp session as an Android client (experimental)")`
  (Go logs the same at warn, `connector.go:46`). Add
  `.with_device_props(device_props)` to the builder. After `build()` and
  **before** `spawn()`:
  ```rust
  // The profile is in-memory only and the library reads it for every login
  // and registration payload; set once before the first connect, it holds
  // for every reconnect of this process.
  bot.client().set_client_profile(profile).await;
  ```
- `config.example.toml`: a commented block like Go's (`connectors/whatsapp/config.example.toml`,
  the `client_mode` paragraph), with `client_mode = "android"`, saying that
  changing it needs a fresh pairing.

Tests (`device.rs`): `each_mode_registers_as_its_android_client` — personal →
platform `ANDROID`, business → `SMB_ANDROID`; both `include_web_info ==
false`, `device == "Desktop"` and `os_version == "0.1"`; props `platform_type == Some(ANDROID_PHONE)` and `os ==
Some("Android")` (Go: `TestUseAndroidClientIdentity`). `config.rs` gains its
first tests: `client_mode_defaults_to_android` (a minimal TOML without the
key), `client_mode_reads_business`, `an_unknown_client_mode_is_refused`.

**Verify**: whatsapp-rust `cargo test` → green, + 4; clippy clean. Mutation:
map `AndroidBusiness` to `android` → the device test fails.
Commit: `feat(whatsapp-rust): register as an Android client, configurable with client_mode`.

### Step 5: Docs

- `README.md`, Rust connector section (131-155): inbound media (photos,
  videos, GIFs, voice notes, documents, stickers) with captions and view-once
  is bridged; outbound is still text (until 054); the `client_mode` key and
  that it needs one re-pair.
- `connectors/whatsapp-rust/config.example.toml`: done in Step 4.

**Verify**: `grep -n 'client_mode' README.md connectors/whatsapp-rust/config.example.toml`
→ a hit in each.
Commit: `docs(whatsapp-rust): inbound media and the Android client identity`.

### Step 6: Gate, dev binary, index, and the owner's live test

1. Gate (alone): whatsapp-rust `cargo test` (Step 0 count + 20, green),
   `cargo clippy … -D warnings` clean, `cargo fmt … --check` clean;
   `git diff --stat 313038d -- connectors/whatsapp-rust/Cargo.lock` → empty
   (no dependency change).
2. Dev binary (mirrors `scripts/install-dev-bins.sh`; check `pgrep` first):
   ```sh
   RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo build --release --manifest-path connectors/whatsapp-rust/Cargo.toml
   mkdir -p scripts/dev-bin/.bin
   install -m 0755 connectors/whatsapp-rust/target/release/wuphf-connector-whatsapp scripts/dev-bin/.bin/wuphf-whatsapp-rust
   ```
   (`scripts/dev-bin/.bin/` is git-ignored; nothing to commit.)
3. `plans/README.md`: row 051 DONE with a notes bullet (test counts, the
   mutation checks, D1's answer, deviations, the owner action below).
   Commit: `docs(plans): mark plan 051 done`.

**For the owner (put this in the notes bullet too):**

- **Restart**: the whatsapp-rust connector only (the `wwr` tab of the dev
  session, or `scripts/dev-bin/wuphf-whatsapp-rust`). The core, Telegram and
  the Go WhatsApp connector are unchanged.
- **Config**: `client_mode` in the connector's own config (the file
  `$WUPHF_WHATSAPP_RUST_CONFIG` points at, else
  `connectors/whatsapp-rust/config.example.toml`): `"android"` (default) for
  a personal account, `"android_business"` for a WhatsApp Business account.
- **Re-pair once** (the identity is sent at pairing and login; agreed):
  1. Stop the connector. On the phone: WhatsApp → Linked devices → remove the
     wuphf device.
  2. Move the session database aside (the `session_database` path, by
     default `connectors/whatsapp-rust/whatsapp-rust.session.db` plus its
     `-shm`/`-wal` files), e.g. to `/tmp/`. It also holds the
     `wuphf_contacts` table; the history sync after pairing refills it.
  3. Start the connector, pair with the QR code (or the pair code when
     `phone_number` is set). Linked devices should now show an Android
     device.
- **Live test**:
  1. From another phone, send the owner a photo with a caption, a photo
     without one, a video, a GIF, a voice note, a PDF and a sticker in a
     chat mirrored to Telegram → each arrives in the mirror topic with its
     media; the caption is the text; the GIF plays as a GIF; the PDF keeps
     its file name.
  2. Send a view-once photo with a caption and a view-once voice note → the
     mirror shows the media with `👁️ View once` (and the caption under it).
     This is the check that the Android identity works: before this plan a
     view-once message did not arrive at all.
  3. In a chat with disappearing messages on, react to a message, edit a
     message, and reply to a message → the reaction, the edit and the reply
     link all reach the mirror.
  4. Watch the connector log for `dropping a WhatsApp media message`; report
     any with its error.
  5. If pairing fails or the phone refuses the device, report the log lines
     from the pairing attempt.

## Test plan

| Test | Where | Proves |
|---|---|---|
| wrapped reaction / edit / reply / revoke / timer | whatsapp-rust normalize | decision 5 |
| media description table, empty fields, fetch / failed fetch | whatsapp-rust media | decision 2, planner decision on failures |
| photo with / without caption, unfetched dropped, document caption, view-once variants, mentions in captions | whatsapp-rust normalize | decisions 2–4 |
| client identity per mode, config parsing | whatsapp-rust device, config | decision 6 |

## Done criteria

- [ ] Every test above passes; every listed mutation fails a test.
- [ ] whatsapp-rust built alone; its `Cargo.lock` unchanged; clippy and fmt
      clean.
- [ ] Dev binary installed; row 051 DONE with notes and the owner's steps.

## STOP conditions

- The drift check fails, or a "must still hold" fact is false.
- A library fact used here is not where the plan says (e.g. no
  `set_client_profile`, `with_device_props`, `ClientProfile::smb_android`, or
  `Downloadable` for one of the five message types at `5ca80c5c`).
- Building the connector needs a dependency change (its `Cargo.lock` would
  move).
- Any test that existed at Step 0 starts failing for a reason this plan does
  not explain.

## Maintenance notes

- **Sealed edits older than 20 minutes are dropped by whatsapp-rust**
  (`src/message/msg_secret.rs:362-375`, WA Web's edit-processing window, on
  the edit's authored time). The Go connector bridges them. Accepted by the
  owner; an edit made long after the message on the main phone therefore
  does not reach the mirror through this connector.
- **The client profile is in-memory only.** It is set once per process before
  `spawn()`; the library keeps it across its own reconnects. If the connector
  ever rebuilds the bot inside one process, it must set it again.
- **Changing `client_mode` needs a fresh pairing**: the account type must
  match the registration, and the device props are sent only at pairing.
- **Media failures drop the message** (logged at `warn`). If drops show up in
  practice, a media-retry request (`src/features/media_reupload.rs`) is the
  library's path for expired media; neither connector uses it today.
- Plan 053 changes how chat and sender ids are canonicalized; the media path
  here does not depend on it.
