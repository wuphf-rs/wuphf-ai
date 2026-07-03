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

Phase status: 1 DONE · 2 DONE · 3 TODO · 4 TODO · 5 TODO · 6 TODO

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

### Phase 3 — observer gRPC read service (TODO)

- `observer.proto`: `ListChats`, `GetMessages(chat, before_ts, limit)`,
  `Subscribe(filter) -> stream`, `SendMessage(...)` (stub until phase 5).
- Implement over `Observer::storage()` + `Observer::subscribe()`; add the
  service in `wuphf-cli/src/main.rs`. Bind local-only + token — it reads all
  chats and (phase 5) can send. Reuse the existing media service for bytes.
- **Verify**: `cargo test` + manual gRPC probe.

### Phase 4 — read-only TUI (TODO)

- New `wuphf-tui` crate (ratatui + tonic client). Chat-list / timeline / input
  panes. `ListChats` on start, `GetMessages` + scrollback, `Subscribe` live
  upsert by `(connector, chat, platform_message_id)` honoring deleted/edited.
- **Milestone**: usable read-only multi-platform viewer.

### Phase 5 — outbound SendRaw (TODO)

- `SendRaw { chat_id, thread_id?, text, reply_to? }` connector command; WA/IG
  gain native send. Core records a sent copy so the existing
  `SentMessageEcho` / `is_sent_copy` path suppresses the echo. Log the send as
  `ObservedDirection::Outgoing`.
- **STOP** if suppressing the outbound echo is not provable in a test — a send
  loop must not be shipped.

### Phase 6 — TUI send/reply (TODO)

- Wire the input box + reply to the observer `SendMessage` RPC.
- **Milestone**: two-way TUI.

## STOP conditions

- Any routing test regresses → observation must be purely additive.
- Media retention (`0003`) pruning deletes blobs still referenced by observed
  messages *and* that is judged a problem → revisit retention (out of scope now;
  media is metadata-only in the TUI).
- Phase 5 echo suppression not demonstrable by test.
