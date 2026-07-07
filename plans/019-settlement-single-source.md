# Plan 019: Single-source dispatch-settlement semantics across storage backends (and fix the memory-backend divergence)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 52c5084..HEAD -- crates/wuphf-storage crates/wuphf-model/src/dispatch.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M (the divergence bugfix alone is S — Step 1 can land standalone)
- **Risk**: MED
- **Depends on**: none (benefits from `plans/004-storage-backend-parity-tests.md`; this plan adds its own targeted parity tests either way)
- **Category**: tech-debt (with a live correctness consequence in the test backend)
- **Planned at**: commit `52c5084`, 2026-07-07

## Why this matters

Acknowledging a dispatch is not a simple state write: it must mark the dispatch
acknowledged, decide whether to record a delivered copy, find the dispatches
blocked on that message at that endpoint, bind the native parent id into them,
and release them to pending. Today that *decision logic* is implemented twice —
once in `InMemoryStorage`, once in `SqliteStorage` — and the two copies have
already diverged: SQLite gained a `records_copy()` guard so that acknowledging
a Reaction/DeliveryAck/SeenReceipt dispatch does not record a sent copy, but
the memory backend never got it. The memory backend therefore flips a message's
*received* copy at the origin endpoint into a *sent* copy when a signal
targeting the origin is acknowledged — exactly the hazard documented on
`DispatchCommand::records_copy`. Since the memory backend is what most tests
run against, tests can pass (or fail) in ways production SQLite would not.

This plan (1) fixes the divergence, (2) moves the settlement *decisions* into
one shared, pure implementation both backends apply, so the next semantic
change cannot drift, and (3) pins the behavior with tests that run against both
backends.

## Current state

- `crates/wuphf-storage/src/lib.rs` — the `Storage` trait;
  `acknowledge_dispatch(dispatch_id, platform_message_id) -> Vec<StoredDispatch>`
  (lines 132–136) returns the released (formerly blocked) dispatches.
- `crates/wuphf-storage/src/shared.rs` — helpers already shared by both
  backends: `dispatch_state(waiting_for)` and
  `bind_native_parent(command, parent, platform_message_id)`.
- `crates/wuphf-storage/src/memory.rs` — `acknowledge_dispatch` at lines
  406–470. **Missing the `records_copy()` guard.**
- `crates/wuphf-storage/src/sqlite.rs` — `acknowledge_dispatch` at lines
  454–540, all inside one sqlx transaction. **Has the guard** at line 479.
- `crates/wuphf-model/src/dispatch.rs` — `DispatchCommand::records_copy()`
  (lines 130–137) with the doc comment explaining why signals must not record
  a copy; `message_id()` (lines 113–123) returns `Some` for signals too, which
  is why the memory backend's `message_id()`-only check is insufficient.

The memory backend today (`memory.rs:419–433`):

```rust
// A retitle carries no logical message, so it records no copy and
// releases no replies; acknowledging it just closes out the dispatch.
let Some(message_id) = dispatch.command.message_id() else {
    return Ok(Vec::new());
};
let endpoint_id = dispatch.command.target_endpoint;
put_copy(
    &mut state,
    MessageCopy {
        message_id,
        endpoint_id,
        platform_message_id: platform_message_id.clone(),
    },
    true,
);
```

`put_copy(..., true)` (memory.rs:599–612) inserts into `sent_copies`, which
feeds `is_sent_copy` — the echo-suppression check — and overwrites the
`copies` entry. For a `DeliveryAck` targeting the message's origin endpoint,
this converts the received copy into a sent copy.

The SQLite backend at the same point (`sqlite.rs:476–486`):

```rust
// Signals that act on an existing copy (reactions, delivery acks, seen
// receipts) and retitles record no copy and release no replies;
// acknowledging them just closes out the dispatch.
if !dispatch.command.records_copy() {
    transaction.commit().await?;
    return Ok(Vec::new());
}
```

After recording the copy, both backends then: query dispatches with
`state == Blocked { waiting_for: message_id }` **and**
`target_endpoint == this dispatch's endpoint`, load the parent `Message`, call
`bind_native_parent`, set state to `Pending`, and return the released list.

Conventions: doc comments explain *why* (see any function in
`crates/wuphf-core/src/acknowledge.rs`); commit style is conventional commits
(`fix: …`, `refactor: …` — see `git log --oneline`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Rust tests (all) | `cargo test --workspace` | exit 0; 52+ tests pass |
| Storage tests only | `cargo test -p wuphf-storage` | exit 0 |
| Core tests only | `cargo test -p wuphf-core` | exit 0; 24 tests pass |

Note: the build needs `protoc` on PATH (`brew install protobuf` on macOS; the
repo's `flake.nix` dev shell also provides it where nix is available).

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-storage/src/memory.rs`
- `crates/wuphf-storage/src/sqlite.rs`
- `crates/wuphf-storage/src/shared.rs`
- `crates/wuphf-storage/src/tests.rs`

**Out of scope** (do NOT touch, even though they look related):
- `crates/wuphf-storage/src/lib.rs` — the `Storage` trait signature stays
  exactly as is; plan 021 restructures the trait and must not conflict.
- `crates/wuphf-core/**` — settlement stays *inside* storage in this plan.
  Hoisting orchestration into the core was considered and rejected: SQLite
  wraps settlement in one transaction, and splitting it across trait calls
  would lose crash-atomicity (a crash between "acknowledged" and "released"
  would strand blocked dispatches forever, since only `Pending` dispatches are
  redelivered on restart).
- `crates/wuphf-model/src/dispatch.rs` — `records_copy()` is already correct.
- `migrations/` — no schema change is needed.

## Git workflow

- Branch: `advisor/019-settlement-single-source` off `v2`.
- Two commits: `fix: record no sent copy when acknowledging signal dispatches
  in the memory backend` (Step 1+tests), then `refactor: share dispatch
  settlement decisions between storage backends` (Steps 2–3).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add the `records_copy()` guard to the memory backend

In `crates/wuphf-storage/src/memory.rs`, `acknowledge_dispatch`: after setting
`DispatchState::Acknowledged`, mirror SQLite — if
`!dispatch.command.records_copy()`, return `Ok(Vec::new())` before `put_copy`
and before the blocked-dispatch scan. Keep the existing
`let Some(message_id) = … else` for the copy construction. Carry over SQLite's
comment explaining why signals record no copy.

**Verify**: `cargo test -p wuphf-storage && cargo test -p wuphf-core` → exit 0.
(Core tests exercise reactions/receipts against the memory backend, e.g.
`read_receipt_on_a_mirror_copy_surfaces_a_seen_receipt_on_the_origin` in
`crates/wuphf-core/src/tests.rs:864`.)

### Step 2: Add a settlement-decision helper to `shared.rs`

Extend `crates/wuphf-storage/src/shared.rs` with pure functions that own the
decisions, so backends only perform I/O:

```rust
/// The copy an acknowledged dispatch records, or `None` for payloads that
/// settle without recording one (signals and retitles). Single source of the
/// records-a-copy decision for both backends.
pub(crate) fn acknowledged_copy(
    command: &DispatchCommand,
    platform_message_id: &str,
) -> Option<MessageCopy> { /* records_copy() + message_id() + construct */ }

/// Release one blocked dispatch against its now-delivered parent: bind the
/// native parent id and flip it to pending.
pub(crate) fn release_blocked(
    dispatch: &mut StoredDispatch,
    parent: Message,
    platform_message_id: String,
) { /* bind_native_parent + state = Pending */ }
```

`release_blocked` wraps the existing `bind_native_parent` plus the
`state = DispatchState::Pending` line both backends currently duplicate.

**Verify**: `cargo build -p wuphf-storage` → exit 0 (helpers compile; may
warn unused until Step 3).

### Step 3: Rewire both backends onto the helpers

- `memory.rs`: replace the guard from Step 1 + manual `MessageCopy`
  construction with `let Some(copy) = acknowledged_copy(...) else { return Ok(Vec::new()) }`;
  replace the per-dispatch `bind_native_parent` + `Pending` assignment with
  `release_blocked`.
- `sqlite.rs`: same substitution inside the existing transaction. The
  transaction boundaries, SQL statements, and commit points must not change.

The observable behavior of both backends must be identical before and after
this step.

**Verify**: `cargo test --workspace` → exit 0, same test count passing as
before Step 1 plus any tests added below.

## Test plan

Add to `crates/wuphf-storage/src/tests.rs`, following the existing pattern of
one shared generic assertion function called by a `memory_*` and a `sqlite_*`
test (model after `observation_log_roundtrips` at `tests.rs:161–175`; use the
existing `command(...)`/`message(...)` builders at `tests.rs:289`/`145`):

1. `settling_a_signal_dispatch_records_no_copy` — enqueue + acknowledge a
   create dispatch to record a *received* copy setup is not needed; simplest:
   put a message, `put_received_copy` at an endpoint, enqueue a
   `DispatchPayload::DeliveryAck` (and one `Reaction`) targeting that endpoint,
   acknowledge it with the copy's platform id, then assert
   `is_sent_copy(endpoint, platform_message_id)` is still **false** and the
   returned released list is empty. Run against **both** backends. This is the
   regression test for the divergence this plan fixes — it must fail on the
   memory backend if Step 1 is reverted.
2. `settling_a_create_releases_blocked_and_records_sent_copy` — enqueue a
   create, enqueue a second dispatch blocked on its message
   (`enqueue_dispatch(cmd, Some(message_id))`), acknowledge the create, assert
   the blocked dispatch comes back `Pending` with the native id bound and
   `is_sent_copy` is now true. Both backends. (Extends the existing
   sqlite-only coverage at `tests.rs:8` to memory.)

**Verification**: `cargo test -p wuphf-storage` → all pass, including 4 new
tests (2 assertions × 2 backends).

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `cargo test --workspace` exits 0
- [ ] `grep -n "records_copy" crates/wuphf-storage/src/memory.rs crates/wuphf-storage/src/sqlite.rs crates/wuphf-storage/src/shared.rs` shows the check present only in `shared.rs`
- [ ] `grep -c "bind_native_parent" crates/wuphf-storage/src/memory.rs crates/wuphf-storage/src/sqlite.rs` → 0 in each (both call `release_blocked` instead)
- [ ] New parity tests exist and pass against both backends
- [ ] `git status` shows no modified files outside the in-scope list
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts under "Current state" don't match the live code (drift).
- Making the backends behaviorally identical requires changing the `Storage`
  trait signature — that belongs to plan 021; report the conflict.
- The new signal-settlement test reveals that some *existing* core test
  depends on the memory backend's buggy sent-copy recording (i.e. a core test
  fails after Step 1). That means real behavior was leaning on the bug —
  report which test, do not "fix" the core.
- SQLite settlement cannot adopt a helper without moving a statement outside
  the transaction.

## Maintenance notes

- Any future dispatch payload variant must be classified in
  `DispatchCommand::records_copy()`; the shared `acknowledged_copy` then
  applies it to both backends automatically — that is the point of this plan.
- Reviewer should scrutinize: SQLite transaction boundaries unchanged; the
  memory backend's released-dispatch filter still matches on *both*
  `waiting_for == message_id` and `target_endpoint`.
- Deferred: hoisting settlement orchestration out of storage entirely
  (rejected here for atomicity reasons — revisit only if a non-transactional
  backend is ever added); the full storage parity suite remains plan 004.
