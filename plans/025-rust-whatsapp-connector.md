# Plan 025: WhatsApp connector on whatsapp-rust (Rust) — handoff

Status: **DONE**
Started 2026-07-27 on branch `v2`.

## Goal

A Rust WhatsApp connector built on `whatsapp-rust` (git pin
`https://github.com/oxidezap/whatsapp-rust`, rev `5ca80c5c` — the same code as
`vendor/whatsapp-rust`, which is reference-only and must **not** become a path
dependency). Text-first v1: QR/pair-code login, durable SQLite session,
inbound text/edit/delete/reaction → core events, outbound
create/edit/delete dispatches, ack + dead-letter semantics identical to the Go
connectors.

## What already exists (do not rebuild)

### `crates/wuphf-connector/` (main workspace member) — compiles clean

Rust counterpart of `connectors/shared/coreclient` (Go). Read the Go file
first; the Rust design mirrors it 1:1.

- `src/sender.rs` — `Failure::{Permanent,Transient}`, `Delivery { message_id,
  thread_id }`, and the `Sender` trait: required `send_message` /
  `edit_message` / `delete_message`, plus default capability methods matching
  the Go fallbacks (`retitle_thread` → permanent dead-letter;
  `apply_reaction` / `ack_delivery` / `mark_seen` → ack-only with the target
  id unchanged).
- `src/client.rs` — `Options { core_address, connector_id, platform,
  supports_threads, supports_topic_create, is_permanent }`, `Client::run(sender,
  inputs, cancel)`:
  - reconnect with exponential backoff 1s→30s (reset on successful
    registration), cancellation via `tokio_util::sync::CancellationToken`;
  - registration frame + connector event frames + ack/failure frames share one
    outbound mpsc (capacity 64) so the core observes them in connector order;
  - dispatches handled sequentially (same as Go): 3 attempts with 500 ms
    delay → success acks, `Failure::Permanent` (or `Options::is_permanent`)
    sends `DispatchFailed`, transient failure returns `SessionError::Delivery`
    which drops the session so the core redelivers;
  - `Rejected` frame or empty frame ends the session with an error.
- `tests/session.rs` — **written but does not compile**; see "Remaining" §1.

### `connectors/whatsapp-rust/` — standalone Cargo workspace, compiles clean

Intentionally its own workspace (like the Go connectors are separate Go
modules): whatsapp-rust's session store links `libsqlite3-sys` 0.37 via
diesel, which cannot coexist in one dependency graph with the 0.28 that sqlx
links for `wuphf-storage`. The root `Cargo.toml` has
`exclude = ["connectors/whatsapp-rust"]`; path deps reach back to
`crates/wuphf-connector` and `crates/wuphf-proto`. **Never** add
`whatsapp-rust` to the root workspace — the links conflict returns.

- `src/config.rs` — TOML: `core_address`, `connector_id`, `session_database`,
  `log_level` (default info), optional `phone_number` (pair-code login).
- `src/normalize.rs` — `MessageContext` → `pb::ClientFrame`, matching the Go
  normalizer's semantics (`connectors/whatsapp/internal/whatsapp/updates.go`):
  - `protocol_message` `MESSAGE_EDIT` → `MessageEditEvent` (original id from
    `key.id`, text from `edited_message`); `REVOKE` → `MessageDeleteEvent`;
  - `reaction_message` → `ReactionEvent` (empty emoji = cleared);
  - text (`MessageExt::text_content`) → `MessageCreateEvent`;
  - chat/sender canonicalized with `jid.to_non_ad()` (device stripped);
  - `participant = sender` always (DM two-side parity), `sender_name` = "Me"
    for from-me, push name otherwise, `" (linked device)"` suffix when
    `sender.device != 0`; reply target from `context_info.stanza_id`;
  - media messages are **dropped with a warn log** (v1, deliberate).
- `src/sender.rs` — `WhatsAppSender` impl of the SDK `Sender` trait:
  - create: attachments → `Failure::permanent` (never partially deliver);
    reply via `build_quote_context_with_info` with own JID from
    `client.get_pn().to_non_ad()`; `reply_to_platform_message_id` without
    `reply_context` → permanent (same as Go);
  - edit → `client.edit_message(chat, id, wa::Message::text(...))`;
  - delete → `client.revoke_message(chat, id, RevokeType::Sender)`;
  - unparseable target JID → `Failure::permanent`; `SendError` → transient.
- `src/main.rs` — clap `--config` (default `config.example.toml`), logging via
  `WUPHF_LOG_LEVEL` env or config, `SqliteStore::new(session_database)`, `Bot`
  builder with `on_qr_code` (terminal QR via `qrcode` crate, raw payload
  fallback), `with_pair_code` when `phone_number` set, `on_message` →
  `Normalizer`, SDK session with `CancellationToken`, ctrl-c → cancel →
  `handle.shutdown()`.

### Root `Cargo.toml` changes already made

- member `crates/wuphf-connector`; `exclude = ["connectors/whatsapp-rust"]`;
- workspace tokio features gained `time` and `signal` (SDK sleeps, connector
  ctrl-c);
- `tokio-util = { version = "0.7", features = ["rt"] }` added.

## Remaining work

### 1. Fix `crates/wuphf-connector/tests/session.rs` (does not compile)

Known defects, all in the test harness — the SDK itself is fine:

- `channels_split` clones `RuntimeChannels.dispatches`
  (`mpsc::Receiver`, no `Clone`). Delete the helper. Restructure
  `Harness::start`: call `Runtime::channelled(core, 8)`, clone
  `channels.inbound` (an `mpsc::Sender`, clonable) **before** moving
  `channels` into `ConnectorHub::new(channels, registrations())`, and store
  only that sender in the harness (plus storage + task handles).
- `wait_for` + `tokio::task::block_in_place` + `Handle::block_on` hack for
  storage assertions: `#[tokio::test]` is a current-thread runtime, so
  `block_in_place` **panics**. Replace with a plain async poll loop:
  `async fn wait_for_dispatch_state(storage, dispatch_id, predicate)` that
  loops `storage.dispatch(dispatch_id).await` + `sleep(20ms)` up to ~2 s.
- Remove the unused `ScriptedSender` struct and the unused/incorrect
  `wait_for_dispatch` helper.
- The third test (`inbound_event_frames_are_forwarded_to_the_core`) has a
  rambling, self-contradictory comment block — rewrite it to one sentence:
  a second SDK session registered as WA sends the event; the TG session under
  test must receive the routed dispatch.
- Keep the three test intents: (a) dispatch → sender → ack settles as
  `DispatchState::Acknowledged`; (b) permanent failure → `DispatchState::Failed`
  and exactly one send attempt (no redelivery); (c) event frame sent via the
  SDK inputs channel routes to the other connector.

Run `cargo test -p wuphf-connector` until green. Harness reference:
`crates/wuphf-runtime/src/tests.rs` (`grpc_transport_delivers_dispatch_to_registered_connector`
and the `registrations()`/`wa_event()` helpers around lines 585–729).

### 2. Add `connectors/whatsapp-rust/config.example.toml`

Model on `connectors/whatsapp/config.example.toml` (Go). Fields:
`core_address = "127.0.0.1:50051"`, `connector_id = "whatsapp-rust"`,
`session_database = "whatsapp-rust.session.db"`, `log_level = "info"`,
commented `# phone_number = "15551234567"`. Comments must state: routing is
owned by the core (`[[rules]]` with WhatsApp JIDs like `12345@s.whatsapp.net`
/ `12345@g.us` as chat ids); the `connector_id` must match a `[[connectors]]`
entry in the core config; WhatsApp has no threads (capabilities are hardcoded
false); `WUPHF_LOG_LEVEL` overrides `log_level`; same session db path =
session restore, delete it + restart to re-pair after logout.

### 3. Docs

- `config/example.toml`: add a **commented-out** `[[connectors]]` example for
  `whatsapp-rust` (id `whatsapp-rust`, platform `whatsapp`). Must stay
  commented — `crates/wuphf-cli/src/main.rs` has config-parse tests pinning
  the file, and an active entry changes the registration allowlist.
- `README.md`: add a short "Rust WhatsApp connector" note under the connector
  build instructions: separate workspace (`cd connectors/whatsapp-rust &&
  cargo build --release`), why it's separate (libsqlite3-sys links conflict),
  pairing via QR or `phone_number`.

### 4. Verification (all must pass)

```sh
cargo fmt --all && (cd connectors/whatsapp-rust && cargo fmt --all)
cargo clippy --workspace --tests          # main workspace
(cd connectors/whatsapp-rust && cargo clippy)
cargo test --workspace                    # needs protoc
(cd connectors/whatsapp-rust && cargo build)
```

Then update this plan's row in `plans/README.md` to DONE.

### 5. Optional manual e2e (needs a real WhatsApp account)

Run the core (`cargo run -p wuphf-cli -- serve --config config/example.toml`
with a `[[connectors]]` entry for `whatsapp-rust`), run the connector, pair
via QR, send a message in a routed chat; observe it on the other platform and
in the TUI (`wuphf-tui`).

## Deliberate v1 limitations (document, don't "fix")

- No media: inbound media messages dropped (warn log), outbound dispatches
  with attachments dead-lettered permanently. Media needs the
  `MediaTransport` client + `Client::upload`/`download` — follow-up work,
  likely with plan 016 (media lifecycle).
- No LID/phone identity-migration reporting and no contact/group name
  resolution (`chat_title` is always `None`; `sender_name` is push name only).
- No receipts, presence, chat history store, `client_mode` (business)
  override.
- whatsapp-rust runs as a **second linked device** alongside the Go whatsmeow
  connector unless that one is retired (see plan 024 for context).

## Key API references (vendored copy at `vendor/whatsapp-rust`, rev 5ca80c5c)

- `Bot` builder + `MessageContext`: `src/bot.rs` (MessageContext at :139)
- send/edit/revoke: `src/send/mod.rs:560`, `src/client/messaging.rs:61`,
  `src/send/actions.rs:14` (`RevokeType`)
- quote context: `wacore/src/proto_helpers.rs:716`
  (`build_quote_context_with_info`)
- own JID: `src/client/accessors.rs:247` (`get_pn`)
- inbound protocol messages (edits rewrapped to legacy shape) arrive via
  `on_message`: `src/message/dispatch.rs`
- prelude exports: `src/lib.rs:147` (`Bot`, `SqliteStore`, `Jid`, `wa`,
  `MessageBuilderExt`, `MessageExt`, …)
