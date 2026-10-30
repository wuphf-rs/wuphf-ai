# Plan 052: whatsapp-rust — owner notices, inbound receipts, and staying up

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
> `git log --oneline 313038d..HEAD -- crates/wuphf-connector crates/wuphf-proto connectors/whatsapp-rust connectors/shared/coreclient connectors/whatsapp/internal/whatsapp`.
> Expected: no commits, or only commits of plans 051 and 053–056 that
> `plans/README.md` marks DONE. Any other commit: re-check every fact below.
> What must still hold (if any is false, STOP; a fact a DONE plan changed as
> its own plan says counts as holding):
> - `crates/wuphf-connector/src/lib.rs` exports `Client, Options,
>   PermanentFailureClassifier, SessionError, MediaClient, Delivery, Failure,
>   Sender` and has no notice helper.
> - `crates/wuphf-proto/client.proto`: `ReceiptEvent { chat_id = 1;
>   thread_id = 2; platform_message_id = 3; observer = 4; from_me = 5;
>   ReceiptKind kind = 6 }` (172-179); `ReceiptKind` DELIVERED = 1, READ = 2
>   (157-161); `NoticeKind` LOGGED_OUT = 1, AUTH_REQUIRED = 2, DEGRADED = 3,
>   RECOVERED = 4 (185-196). The core already maps receipts and notices (no
>   core change in this plan).
> - `connectors/whatsapp-rust/src/main.rs`: `on_connected` (126-128) and
>   `on_logged_out` (129-134) only log; the chat-event handler is registered
>   with `on_event_for` at 144-155; the final `tokio::select!` (187-195) ends
>   the process with `anyhow::bail!("WhatsApp client stopped unexpectedly")`
>   when the bot handle finishes (194).
> - whatsapp-rust library (`~/.cargo/git/checkouts/whatsapp-rust-6e26428647c827f3/5ca80c5`):
>   `wacore/src/types/events.rs` — `Event::Connected` (629), `LoggedOut`
>   (633; struct 1037 `{ on_connect: bool, reason: ConnectFailureReason }`),
>   `ClientOutdated` (638), `Receipt` (655; struct 1238 `{ source:
>   MessageSource, message_ids, timestamp, r#type: ReceiptType, offline }`),
>   `StreamReplaced` (721), `TemporaryBan` (722; struct 1083 `{ code:
>   TempBanReason, expire: chrono::Duration }`), all `#[non_exhaustive]` with
>   `bon` builders; `EventKind` (211) has `Connected`, `LoggedOut`,
>   `ClientOutdated`, `Receipt`, `StreamReplaced`, `TemporaryBan`.
>   `wacore/src/types/presence.rs:29` `ReceiptType { Delivered, Sent, Sender,
>   Retry, EncRekeyRetry, Read, ReadSelf, Played, PlayedSelf, ServerError,
>   Inactive, PeerMsg, HistorySync, Other(String) }`.
>   `src/client/lifecycle.rs:332-447`: the client's run loop reconnects with
>   backoff and exits only when `enable_auto_reconnect` is false, which the
>   library sets on a logout, a replaced stream, a `401`/`409`/`516` stream
>   error (`src/client/node_io.rs:1286, 1312, 1324, 1336`) and on any connect
>   failure that should not be retried (`:1441`; temporary ban, outdated
>   client, logout). `src/bot.rs:433-472`: the `BotHandle` future resolves
>   when that loop exits.
> - Go WhatsApp (the reference): `connectors/whatsapp/internal/whatsapp/connector.go:94-130`
>   (event switch; session notices go out through `coreclient.SendNotice`),
>   `:201-228` `sessionNotice`; `updates.go:300-340` `HandleReceipt` and
>   `receiptKind`; `connectors/shared/coreclient/notice.go:15-33` `Notice` and
>   `SendNotice` (non-blocking: drops the notice when the channel is full).
>   whatsmeow `types/events/events.go:156-161` `TemporaryBan.String`, `:188-215`
>   `ConnectFailureReason` messages and `String` (module cache
>   `~/.local/share/golang/pkg/mod/go.mau.fi/whatsmeow@v0.0.0-20260525123251-933deb5f2ee9`).

## Status

- **Priority**: P2 — a logged-out Rust connector exits instead of telling the
  owner, and its DM copies never show delivered / read marks
- **Effort**: S–M
- **Risk**: LOW — additive handlers; the process lifetime changes
- **Depends on**: — (none)
- **Category**: feature / reliability
- **Planned at**: commit `313038d`, 2026-10-31

## Why this matters

When WhatsApp ends the Rust connector's session (a logout from the phone,
another client taking over, a temporary ban, an outdated client), the library
stops its run loop, the bot handle finishes, and `main.rs:194` ends the
process. The core sees a dropped connector and nothing else: the owner is
never told why the bridge went quiet, which is exactly what the Go
connector's session notices exist for. And the Rust connector reports no
receipts, so a message the owner sends from Telegram into a WhatsApp DM never
gets the 👍 (delivered) and 👀 (read) marks the Go connector produces.

Plan 2 of 6 (051–056) bringing the Rust connector to parity with the Go one.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Notices on `LoggedOut`, `StreamReplaced`, `ClientOutdated`, `TemporaryBan` and `Connected` (recovered), with Go's kinds and wording (`connector.go:201-228`), sent through a **new notice helper in the Rust SDK** (`crates/wuphf-connector`), mirroring `coreclient.Notice` / `SendNotice`, with its own tests. |
| 2 | A `ReceiptEvent` from `Event::Receipt`, DMs only, delivered and read only, like Go's `HandleReceipt`. |
| 3 | The connector stays up when the WhatsApp client stops, instead of bailing; the library's own auto-reconnect handles every recoverable drop, so the client stopping means it will not come back without the owner. |
| 4 | Owner Q49 (2026-10-31): an inbound media message that cannot be downloaded (051 drops it and stays up) also raises an owner notice, so the drop is never silent. A new notice kind `NOTICE_KIND_MEDIA_UNAVAILABLE`, which bypasses the per-connector health state machine exactly like 045's `NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN` (`crates/wuphf-model/src/connector.rs:49-72`, `crates/wuphf-runtime/src/grpc.rs:274`). The detail names the chat and sender and the media kind. The Go connector gets the same notice (and stops ending its run on a download failure) in a later follow-up plan. |

### Planner decisions (within the settled scope)

- **Detail texts match Go's.** `LoggedOut`: "WhatsApp ended the session", or
  on connect "WhatsApp rejected the session on connect (<code>: <message>)"
  with whatsmeow's reason messages (copied into a small table, since the
  library's `ConnectFailureReason` has a `code()` but no message);
  `StreamReplaced`: "another WhatsApp client replaced this session";
  `ClientOutdated`: "WhatsApp rejected this client as outdated; upgrade the
  connector"; `TemporaryBan`: whatsmeow's `TemporaryBan.String()` ("You've
  been temporarily banned: <code>: <reason>. The ban expires in <duration>",
  the duration in Go's `24h0m0s` format, the "expires" clause omitted when it
  is 0); `Connected`: empty detail.
- **The SDK helper is non-blocking like Go's `SendNotice`** (`try_send`; a
  full or closed channel drops the notice with a `debug` log). The library
  runs each handler on its own task, so blocking would be harmless here, but
  the helper is shared and other Rust connectors may call it from places
  that must not block.
- **Receipt mapping**: `Delivered` → DELIVERED, `Read` → READ; every other
  type (`Sent`, the library's downgraded delivery for a LID peer that cannot
  receive; `ReadSelf`; `Played`; …) is dropped, as Go drops everything but
  delivered and read. One event per message id. Group receipts
  (`source.is_group`, or a group chat JID) are dropped. Chat and observer ids
  go through the connector's `canonical` (device stripped); plan 053 later
  routes them through its identity resolver like every other id.
- **The process now ends only on Ctrl-C or when the core session returns**
  (`Client::run` returns only on cancellation). The run loop's select moves
  into a small `supervise` function so the "WhatsApp stopped, keep going"
  rule is unit-tested with plain futures.

## Current state

- **SDK** (`crates/wuphf-connector`): session client, `Sender` trait,
  `MediaClient`. Nothing builds owner-notice frames; 045's
  `TimerFallback` builds its `DisappearingTimerUnknown` notice by hand
  (`connectors/whatsapp-rust/src/disappearing.rs:273-279`) and sends it with
  its own `emit_without_blocking` (292-306, spawns a task when the channel is
  full). Leave that as it is: it is a different notice with its own gate.
- **Rust connector events**: `main.rs` registers `on_message` (135-138) and
  `ChatEvents` for `GroupUpdate`, `HistorySync`, `PictureUpdate`,
  `ContactUpdate` (144-155). Handlers get `(Arc<Event>, Arc<Client>)` and run
  on their own tasks (bot.rs:885-888), so awaiting the input channel is safe
  in them.
- **Go tests**: `notice_test.go` (`TestSessionNoticeMapsSessionLoss`,
  `TestSessionNoticeCarriesTheConnectFailureReason`), `updates_test.go`
  (`TestHandleReceiptEmitsReceiptForDM`, `TestHandleReceiptSkipsGroupAndUnsurfaced`),
  `connectors/shared/coreclient/client_test.go` covers `SendNotice`.

## Commands you will need

```sh
pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv   # must print nothing

export CARGO_BUILD_JOBS=2
nice -n 19 cargo test -p wuphf-connector
nice -n 19 cargo clippy -p wuphf-connector --all-targets -- -D warnings
cargo fmt --check

# whatsapp-rust, alone
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo test --manifest-path connectors/whatsapp-rust/Cargo.toml
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --manifest-path connectors/whatsapp-rust/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path connectors/whatsapp-rust/Cargo.toml --check
```

## Scope

In: `crates/wuphf-connector/src/{lib.rs,notice.rs (new)}`,
`connectors/whatsapp-rust/src/{main.rs,health.rs (new),receipts.rs (new)}`,
`README.md` (Rust connector section), `plans/README.md`.

Out: media (051, 054, 055), identity (053), settings (056), the core, the
proto, the Go connector, `.env.local`, `config.local.toml`,
`*.session.json`, `.claude/`, `connectors/instagram/**`, `plans/048-*`.

## Git workflow

Commit directly on `v2`, one commit per step. Do not push. No branches.

## Steps

### Step 0: Baseline

Drift check. `nice -n 19 cargo test -p wuphf-connector` → record the count
(6 at `313038d`: session 4, media 2). Then, alone, the whatsapp-rust tests →
record the count.

### Step 1: SDK notice helper

New `crates/wuphf-connector/src/notice.rs`, exported from `lib.rs`
(`pub use notice::{notice, send_notice};`, and mention it in the crate doc):

```rust
//! Owner notices: the counterpart of the Go `coreclient.Notice` /
//! `SendNotice`. A connector that loses its platform session otherwise just
//! goes quiet, which looks identical to "nobody is messaging you"; the core
//! relays a notice to the owner through the connectors that still work.
//!
//! Report only what happened. The wording the owner reads, which chats it
//! reaches, and whether a repeat is worth sending are the core's decisions.

/// An owner-notice frame.
pub fn notice(kind: pb::NoticeKind, detail: impl Into<String>) -> pb::ClientFrame;

/// Queue a notice on the connector's input channel without waiting: a full
/// or closed channel drops it (logged at debug). A notice is best-effort by
/// design and must never block or fail its caller. Returns whether it was
/// queued.
pub fn send_notice(
    inputs: &mpsc::Sender<pb::ClientFrame>,
    kind: pb::NoticeKind,
    detail: impl Into<String>,
) -> bool;
```

Tests (`notice.rs` `#[cfg(test)]`, `#[tokio::test]` where a runtime is
needed):

1. `a_notice_frame_carries_kind_and_detail`.
2. `send_notice_queues_when_there_is_room` — channel of 1 → `true`, the
   frame is received.
3. `send_notice_never_blocks_on_a_full_channel` — channel of 1 already full
   → returns `false` at once (wrap the call in
   `tokio::time::timeout(Duration::from_millis(100), async { … })` and
   assert it completes).
4. `send_notice_tolerates_a_closed_channel` — receiver dropped → `false`, no
   panic.

**Verify**: `nice -n 19 cargo test -p wuphf-connector` → Step 0 count + 4;
clippy clean. Mutation: replace `try_send` with `blocking_send` → test 3
fails (it blocks or panics inside the runtime). Restore.
Commit: `feat(connector-sdk): owner notice helper`.

### Step 2: Session notices in the connector

New `connectors/whatsapp-rust/src/health.rs`:

```rust
//! Session health: the WhatsApp events that stop this session from bridging
//! become owner notices, like the Go connector's `sessionNotice`. Without
//! them a logged-out connector is silent, which looks the same as nobody
//! writing.

/// The notice a WhatsApp event calls for, if any.
pub fn session_notice(event: &Event) -> Option<(pb::NoticeKind, String)> {
    match event {
        Event::LoggedOut(out) => Some((LoggedOut, if out.on_connect {
            format!("WhatsApp rejected the session on connect ({})", connect_failure_text(out.reason))
        } else {
            "WhatsApp ended the session".into()
        })),
        // Another client took the session over; this one is done until paired again.
        Event::StreamReplaced(_) => Some((LoggedOut, "another WhatsApp client replaced this session".into())),
        // The fix is a connector upgrade, not a re-pair.
        Event::ClientOutdated(_) => Some((AuthRequired, "WhatsApp rejected this client as outdated; upgrade the connector".into())),
        Event::TemporaryBan(ban) => Some((Degraded, temporary_ban_text(ban))),
        // A reconnect after trouble tells the owner the bridge is back; the
        // core drops it when nothing was reported wrong.
        Event::Connected(_) => Some((Recovered, String::new())),
        _ => None,
    }
}

/// whatsmeow's `ConnectFailureReason.String()`: "<code>: <message>".
fn connect_failure_text(reason: ConnectFailureReason) -> String;   // table of whatsmeow's messages by code, else "unknown error"
/// whatsmeow's `TemporaryBan.String()`.
fn temporary_ban_text(ban: &TemporaryBan) -> String;
/// A whole number of seconds in Go's `time.Duration` format: "24h0m0s",
/// "5m0s", "30s", "0s".
fn go_duration(seconds: i64) -> String;

/// Turns health events into notices on the connector's input channel.
#[derive(Clone)]
pub struct Health { inputs: mpsc::Sender<pb::ClientFrame> }
impl Health {
    pub async fn handle(&self, event: Arc<Event>) {
        if let Some((kind, detail)) = session_notice(&event) {
            warn!(?kind, detail, "WhatsApp session health changed");
            wuphf_connector::send_notice(&self.inputs, kind, detail);
        }
    }
}
```

The `connect_failure_text` table (whatsmeow `events.go:188-197`): 401 "logged
out from another device", 402 "account temporarily banned", 403 "primary
device was logged out", 405 "client is out of date", 406 "logged out for
unknown reason", 409 "client user agent was rejected", 413 "messenger crypto
auth token has expired", 414 "messenger crypto auth token is invalid"; any
other code "unknown error". `temporary_ban_text` uses the library's
`TempBanReason` `Display` ("<code>: <reason>", the same shape as whatsmeow's).

`main.rs`: register `.on_event_for(&[EventKind::LoggedOut,
EventKind::StreamReplaced, EventKind::ClientOutdated, EventKind::TemporaryBan,
EventKind::Connected], move |event, _client| { let health = health.clone();
async move { health.handle(event).await } })`. Keep the `on_connected` and
`on_logged_out` log lines (the logout one keeps telling the operator to delete
the session database).

Tests (`health.rs`; build events with their `bon` builders, e.g.
`LoggedOut::builder().on_connect(true).reason(ConnectFailureReason::LoggedOut).build()`
— if a builder is not public at `5ca80c5c`, test the pure text helpers and a
`notice_for(kind_of_event, …)` split over the parts instead, as 045 did for
`GroupUpdate`):

1. `session_loss_maps_to_go_s_notice_kinds` — table: `LoggedOut` →
   LOGGED_OUT; `StreamReplaced` → LOGGED_OUT; `ClientOutdated` →
   AUTH_REQUIRED; `TemporaryBan` (1 hour) → DEGRADED; `Connected` →
   RECOVERED with empty detail; a `Receipt` or `Disconnected` event → `None`
   (Go: `TestSessionNoticeMapsSessionLoss`).
2. `a_rejected_connect_carries_the_reason` — on-connect logout with reason
   401 → detail `WhatsApp rejected the session on connect (401: logged out
   from another device)` (Go: `TestSessionNoticeCarriesTheConnectFailureReason`).
3. `a_ban_reads_like_whatsmeow` — code 101, 24 hours → `You've been
   temporarily banned: 101: you sent too many messages to people who don't
   have you in their address books. The ban expires in 24h0m0s`; expire 0 →
   no "expires" clause.
4. `durations_print_like_go` — 0 → `0s`, 30 → `30s`, 300 → `5m0s`, 3661 →
   `1h1m1s`, 86400 → `24h0m0s`.

**Verify**: whatsapp-rust tests → Step 0 count + 4, green. Mutation: map
`StreamReplaced` to `Degraded` → test 1 fails. Restore.
Commit: `feat(whatsapp-rust): tell the owner when the WhatsApp session is lost or back`.

### Step 2b: Tell the owner about media that could not be downloaded

Decision 4. Files:
- `crates/wuphf-proto/client.proto`: `NOTICE_KIND_MEDIA_UNAVAILABLE = 6;` (next free
  value; check) with a comment. Regenerate the Go bindings the way 045 Step 1
  did (inside `nix develop`).
- `crates/wuphf-model/src/connector.rs`: `NoticeKind::MediaUnavailable`, its
  wording (`"📎 A WhatsApp media message could not be downloaded"`, the
  detail appended), and excluded from health tracking next to
  `DisappearingTimerUnknown`.
- `crates/wuphf-runtime/src/grpc.rs`: map the wire kind, as for
  `DisappearingTimerUnknown`.
- `connectors/whatsapp-rust`: where 051 logs `dropping a WhatsApp media
  message`, also `send_notice(MediaUnavailable, "<kind> from <sender> in
  <chat title or id>: <error>")`, non-blocking (the 045 deadlock note).

Tests: core `a_media_notice_never_changes_health` (two notices both delivered,
the connector's health state unchanged); runtime wire mapping; whatsapp-rust
`a_failed_download_tells_the_owner` (the notice frame and its detail).
Mutation: track the new kind as health → the core test fails. Restore.

**Verify**: `cargo test -p wuphf-model -p wuphf-core -p wuphf-runtime` and the
whatsapp-rust tests, green; `proto/go` builds.
Commit: `feat: tell the owner when WhatsApp media cannot be downloaded`.

### Step 3: Inbound receipts

New `connectors/whatsapp-rust/src/receipts.rs`:

```rust
//! Delivery and read receipts, reported like the Go connector's
//! `HandleReceipt`: direct chats only (group receipts are per-participant
//! noise), delivered and read only. The core turns them into 👍 and 👀 on the
//! message's other copies and ignores the owner's own (`from_me`).

/// The receipt kinds the bridge surfaces.
pub fn receipt_kind(kind: &ReceiptType) -> Option<pb::ReceiptKind>;

/// One event per message id; none for a group.
pub fn receipt_events(
    source: &MessageSource,
    message_ids: &[String],      // the receipt's ids as strings
    kind: pb::ReceiptKind,
) -> Vec<pb::ReceiptEvent>;      // chat = canonical(source.chat), observer = canonical(source.sender), from_me = source.is_from_me

#[derive(Clone)]
pub struct Receipts { inputs: mpsc::Sender<pb::ClientFrame> }
impl Receipts {
    /// An event handler: awaiting the input channel is safe here.
    pub async fn handle(&self, event: Arc<Event>) { … }
}
```

`main.rs`: `.on_event_for(&[EventKind::Receipt], …)` calling
`Receipts::handle`.

Tests (`receipts.rs`; `MessageSource` is `Default` with public fields):

1. `a_dm_receipt_reports_each_message` — DM, `Delivered`, two ids → two
   events, kind DELIVERED, chat and observer the peer, `from_me` false (Go:
   `TestHandleReceiptEmitsReceiptForDM`).
2. `a_read_receipt_is_read` — `Read` → READ.
3. `groups_and_other_types_are_dropped` — a group source → no events;
   `Sent`, `ReadSelf`, `Played`, `Sender`, `Retry` → `receipt_kind` is `None`
   (Go: `TestHandleReceiptSkipsGroupAndUnsurfaced`).
4. `devices_are_stripped_and_from_me_is_kept` — a sender with device 3 →
   observer without it; `is_from_me` → `from_me: true`.

**Verify**: whatsapp-rust tests → + 4, green. Mutation: report group
receipts → test 3 fails. Restore.
Commit: `feat(whatsapp-rust): report delivered and read receipts in direct chats`.

### Step 4: Stay up when the WhatsApp client stops

`main.rs`: replace the final `tokio::select!` with a call to a new function
(in `main.rs` or a small `src/supervise.rs`):

```rust
/// How the connector stopped.
pub enum Stop { SessionEnded(Result<(), SessionError>), Shutdown }

/// Runs until the core session returns or `shutdown` fires. The WhatsApp
/// client finishing does not end the connector: the library reconnects by
/// itself after every recoverable drop, so it finishes only when the session
/// is gone for good (a logout, a replaced stream, a ban, an outdated
/// client). The core session stays up so the notice of why reaches the core
/// and the owner; `on_whatsapp_stopped` runs once, then the loop keeps
/// waiting.
pub async fn supervise<S, W, Q>(
    session: S, whatsapp: W, shutdown: Q, on_whatsapp_stopped: impl FnOnce(),
) -> Stop
where
    S: Future<Output = Result<(), SessionError>>,
    W: Future<Output = ()>,
    Q: Future<Output = ()>;
```

In `main`: `match supervise(&mut session, &mut handle, ctrl_c, || error!("the
WhatsApp client stopped and will not reconnect (see the notice above); the
connector stays up so the owner is told — fix the session and restart it"))
.await`; on `Shutdown`, `cancel.cancel()` and await the session as today; on
`SessionEnded(result)`, propagate `result?`. Then `handle.shutdown().await`
as today (it returns at once when the client already stopped).

Tests (`#[tokio::test]`, plain futures — `std::future::pending()`,
`std::future::ready(..)`, `tokio::sync::oneshot`):

1. `the_whatsapp_client_stopping_does_not_stop_the_connector` — WhatsApp
   ready at once, session pending, shutdown fired later from a oneshot →
   `Stop::Shutdown`, and the callback ran exactly once.
2. `the_session_ending_ends_the_connector` — session ready with
   `Ok(())` → `SessionEnded(Ok(()))`.
3. `shutdown_wins_while_everything_runs` — shutdown ready, others pending →
   `Shutdown`, callback not run.

**Verify**: whatsapp-rust tests → + 3, green; clippy clean. Mutation: return
from `supervise` when WhatsApp stops → test 1 fails. Restore.
Commit: `fix(whatsapp-rust): stay connected to the core when the WhatsApp client stops`.

### Step 5: Docs

`README.md`, Rust connector section: the connector reports session loss and
recovery as owner notices and delivered / read receipts in DMs; it stays up
after a logout so the notice is delivered (restart it after re-pairing).

**Verify**: `grep -n 'notice' README.md` shows the new sentence in the Rust
section.
Commit: `docs(whatsapp-rust): owner notices and receipts`.

### Step 6: Gate, dev binary, index, and the owner's live test

1. Gate: `cargo test -p wuphf-connector` (Step 0 + 4), clippy and `cargo fmt
   --check` clean; then alone, whatsapp-rust test (Step 0 + 11), clippy, fmt;
   `git diff --stat 313038d -- connectors/whatsapp-rust/Cargo.lock Cargo.lock`
   → empty.
2. Dev binary (check `pgrep` first):
   ```sh
   RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo build --release --manifest-path connectors/whatsapp-rust/Cargo.toml
   install -m 0755 connectors/whatsapp-rust/target/release/wuphf-connector-whatsapp scripts/dev-bin/.bin/wuphf-whatsapp-rust
   ```
   The core is unchanged (the SDK is linked only into Rust connectors), so
   `wuphf-server` is not rebuilt.
3. `plans/README.md`: row 052 DONE with notes. Commit:
   `docs(plans): mark plan 052 done`.

**For the owner:**

- **Restart**: the whatsapp-rust connector only (`wwr`).
- **Config**: none.
- **Live test**:
  1. Restart the connector → the log shows the `Connected` notice going out;
     no message reaches Telegram (the core drops a recovery with nothing
     wrong before it).
  2. From Telegram, write into a mirrored WhatsApp DM → when the contact's
     phone receives it the Telegram message gets 👍; when they open the chat
     it gets 👀. In a mirrored group nothing is marked (DMs only).
  3. On the phone, WhatsApp → Linked devices → remove the wuphf device →
     within seconds the owner gets the core's logged-out notice for
     `whatsapp-rust` with the detail "WhatsApp ended the session", through
     Telegram, and the connector process keeps running (the `wwr` tab is
     still alive and the log says it stays up).
  4. Stop the connector, re-pair as in plan 051's script (move the session
     database aside, start, scan), and check the owner gets the core's
     "working again" notice once it connects.

## Test plan

| Test | Where | Proves |
|---|---|---|
| notice frame, queued, never blocks, closed channel | wuphf-connector | decision 1 (SDK helper) |
| event → kind table, connect-failure detail, ban text, Go durations | whatsapp-rust health | decision 1 |
| DM receipts, read, groups / other types dropped, device strip, from_me | whatsapp-rust receipts | decision 2 |
| WhatsApp stopping keeps the connector, session end ends it, shutdown | whatsapp-rust supervise | decision 3 |

## Done criteria

- [ ] Every test above passes; every listed mutation fails a test.
- [ ] No lock file changed; clippy and fmt clean; whatsapp-rust built alone.
- [ ] Dev binary installed; row 052 DONE with notes and the owner's steps.

## STOP conditions

- The drift check fails, or a "must still hold" fact is false.
- An event this plan maps (`LoggedOut`, `StreamReplaced`, `ClientOutdated`,
  `TemporaryBan`, `Connected`, `Receipt`) or its `EventKind` is missing at
  `5ca80c5c`, or `ReceiptType` has no `Delivered` / `Read`.
- The core refuses or ignores a `ReceiptEvent` or notice frame from this
  connector in the live test (that would be a core gap, not this plan's).
- Any test that existed at Step 0 starts failing for a reason this plan does
  not explain.

## Maintenance notes

- **After a logout the connector stays up but cannot send.** Dispatches to it
  fail transiently, the SDK drops and re-opens the core session with backoff
  (1 s doubling to 30 s), and the core redelivers them after the owner
  re-pairs and restarts. The Go connector behaves the same.
- **A temporary ban ends the client too** (the library, like whatsmeow, does
  not reconnect after a `402` connect failure). Restart the connector once
  the ban expires.
- **Notices are dropped when the input channel is full**, as in Go. The
  channel holds 64 frames and drains while the core session is up; a notice
  lost during a core outage is replaced by the next `Connected` notice.
- 045's `TimerFallback` keeps its own non-dropping emitter for the
  unknown-timer notice (it spawns a task when the channel is full); moving it
  to the SDK helper would change it to dropping, so leave it.
- Plan 053 routes receipt chat and observer ids through its identity
  resolver; until then they are only device-stripped, like every other id in
  this connector.
