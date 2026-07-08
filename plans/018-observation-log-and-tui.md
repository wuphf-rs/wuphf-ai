# Plan 018: Observation log + multi-platform TUI

> **Executor instructions**: This is a phased feature plan, not a single task.
> Phases are independently shippable and land in order. Implement one phase per
> change set, run the verification for that phase, and update the phase's status
> box before starting the next. If a STOP condition fires, stop and report.

## Status

- **Priority**: P2
- **Effort**: L (6 phases)
- **Risk**: MEDIUM (new subsystem; phases 5–6 add outbound sending)
- **Depends on**: none (additive; routing untouched)
- **Category**: feature
- **Planned at**: commit `beed586`, 2026-07-01

Phase status: 1 DONE · 2 DONE · 3 DONE · 4 DONE · 5 DONE · 6 TODO

## Why this matters

The core already persists every *bridged* message (`messages` table), but a chat
that matches no routing rule is dropped at `resolve_origin`
(`crates/wuphf-core/src/resolve.rs:20`) → `IngestOutcome::Ignored` → never
stored. A TUI that shows "everything across all platforms" needs capture of
*every* inbound message, bridged or not. Nothing today exposes a read/subscribe
API for a passive observer either — connectors are routing *endpoints*, not
viewers.

Decisions taken with the user (2026-07-01):

- **Scope**: capture every chat the connector sees, not just bridged ones.
- **Direction**: two-way — the TUI can also send/reply (phases 5–6).
- **Stack**: Rust + ratatui, new `wuphf-tui` crate (phase 4+).
- **Media**: out of scope for the TUI for now (metadata only; no rendering).
- **Unofficial send risk** (WA/IG): accepted.

## Architecture

Two independent subsystems, so bridging (routing) and observation (the TUI read
model) never entangle:

```
connectors ──inbound──► ingest_event ──┬──► core.ingest ──► routing (bridge, unchanged)
                                        └──► Observer.observe ──► observation log (SQLite)
                                                              └──► broadcast (live)
                                                                       ▲
                               wuphf-tui (ratatui) ──gRPC──► observer service
                                        │                     (read: chats/history/subscribe)
                                        └──gRPC SendMessage──► core ──► SendRaw ──► connector (outbound)
```

The Observer never consults routing, so it is inherently capture-all. It lives
in `wuphf-runtime` (a runtime concern), touches persistence only through the
`Storage` trait (per `AGENTS.md`), and keeps `wuphf-core` free of tokio channels.

## Phases

### Phase 1 — observation log storage (DONE, commit `22e7984`)

- `crates/wuphf-model/src/observation.rs`: `ObservedMessage`, `ObservedChat`,
  `ObservedDirection`, `ObservedMessage::preview()`. Re-export from `lib.rs`.
- `migrations/0008_observation_log.sql`: `observed_messages` (PK
  `connector_id, chat_id, platform_message_id`; index on
  `connector_id, chat_id, timestamp_ms`) and `observed_chats` (PK
  `connector_id, chat_id`; index on `last_message_ts`). JSON payload columns.
- `Storage` trait + both backends:
  - `log_observed(msg, chat_title)` — upsert message row + chat summary.
  - `mark_observed_edited(...) -> Option<ObservedMessage>` — set text, flag edited.
  - `mark_observed_deleted(...) -> Option<ObservedMessage>` — flag deleted.
  - `list_observed_chats() -> Vec<ObservedChat>` — most-recent first.
  - `observed_messages(connector, chat, before_ts, limit)` — newest-first page.
- Parity tests in `crates/wuphf-storage/src/tests.rs` (SQLite + InMemory).
- **Verify**: `cargo test -p wuphf-storage -p wuphf-model`.

### Phase 2 — ingest tap + live broadcast (DONE, commit `8a7df54`)

- `Core::storage()` and `Core::connector_platform(&ConnectorId)` accessors.
- `crates/wuphf-runtime/src/observation.rs`: `Observer<S>` holding `Arc<S>` +
  `broadcast::Sender<ObservedMessage>`; `observe(&event, platform)` (create →
  log+broadcast; edit/delete → update+broadcast; reaction/receipt → ignored),
  `subscribe()`, `storage()`.
- Wire into `Runtime`: build in `channelled`, thread through the ingest worker,
  call from `ingest_event` **before** `core.ingest` so unrouted chats are still
  captured. Observation failure logs a warning; it never aborts ingest.
- Return the `Arc<Observer<S>>` from `channelled` for later phases.
- **Verify**: `cargo test -p wuphf-runtime` (Observer unit tests: capture +
  broadcast, edit/delete update).

### Phase 3 — observer gRPC read service (DONE, commit `18f37dc`)

- `observer.proto`: `ListChats`, `GetMessages(chat, before_ms, limit)`,
  `SubscribeMessages(filter) -> stream`, `SendMessage(...)` (returns
  `unimplemented` until phase 5). Registered in the proto build.
- `GrpcObserverService` over `Observer::storage()` + `Observer::subscribe()`;
  added to the same gRPC server in `wuphf-cli/src/main.rs`. Guarded by
  `bearer_auth` (config `observer_token`; unset = allow, with a bind-local
  warning). Media bytes reuse the existing `MediaTransport`.
- Go bindings intentionally not regenerated — no Go connector consumes the
  observer; the phase 4 client is Rust.
- **Verified**: `cargo test -p wuphf-runtime` (4 service tests: list/page,
  arg validation, filtered live stream, send unimplemented).

### Phase 4 — read-only TUI (DONE, commit `52c5084`)

- New `wuphf-tui` crate (ratatui + crossterm + tonic client): chat-list /
  timeline / input panes + status bar. `ListChats` on start, `GetMessages` on
  select, `PgUp`-at-top scrollback, `SubscribeMessages` live upsert by
  `(connector, chat, platform_message_id)` honoring deleted/edited; the chat
  list re-sorts on activity. `--server` / `--token` args.
- State (`app.rs`) is pure and unit-tested (key→`Action`, live upsert, chat
  re-pin); rendering (`ui.rs`) and I/O (`client.rs`) are separate. Input box is
  a read-only placeholder until phase 6.
- **Milestone reached**: usable read-only multi-platform viewer.

### Phase 5 — outbound raw send (DONE, commit pending)

- Scope shrank at implementation time: WA/IG connectors **already** handle
  `MessageCreateDispatch` (`whatsapp/sender.go`, `instagram/sender.go`) and the
  `SendMessage` RPC shape already existed in `observer.proto`, so no Go or proto
  command changes were needed. A raw send is an ordinary `MessageCreate`
  dispatch enqueued outside routing.
- `Core::send_raw(RawOutbound) -> RawSendOutcome` (`wuphf-core/src/send.rs`):
  resolves the target through `resolve_origin` (a rule-matched chat mints its
  full topology; an unrouted chat gets a standalone endpoint + solo replica
  set), best-effort reply quoting, and — deliberately — **fans out to the
  chat's mirrors** via `route_and_record`, because the platform echo that would
  normally replicate the message is suppressed.
- Echo suppression is free: the acknowledgement records a sent copy, so the
  platform's echo hits `is_sent_copy` → `Ignored(SentMessageEcho)`. Proven by
  `raw_send_echo_is_suppressed_after_acknowledgement` (the STOP gate).
- Observation: `Observer::track_outbound/settle_outbound/discard_outbound` —
  the ack settles an `ObservedDirection::Outgoing` row (logged + broadcast +
  oneshot to the RPC waiter). `Storage::log_observed` now returns the stored
  row and keeps `Outgoing` on upsert, so an echoed own-send never flips to
  incoming (parity-tested in both backends).
- `RawMessageSender` (runtime handle) + `GrpcObserverService::with_sender`
  implement the `SendMessage` RPC: waits up to 10s for the ack's native id,
  returns empty id on timeout (send still proceeds), `Aborted` on permanent
  dispatch failure. `wuphf-cli` wires it; a service built without a sender
  stays read-only (`Unimplemented`).

### Phase 6 — TUI send/reply (TODO)

- Wire the input box + reply to the observer `SendMessage` RPC.
- **Milestone**: two-way TUI.

## STOP conditions

- Any routing test regresses → observation must be purely additive.
- Media retention (`0003`) pruning deletes blobs still referenced by observed
  messages *and* that is judged a problem → revisit retention (out of scope now;
  media is metadata-only in the TUI).
- Phase 5 echo suppression not demonstrable by test.
