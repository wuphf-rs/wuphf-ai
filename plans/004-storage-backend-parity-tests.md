# Plan 004: Run every storage test against both backends (SQLite + InMemory)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-storage/src/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW (test-only)
- **Depends on**: none (do before plans 005 and 006, which change storage)
- **Category**: tests
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

`wuphf-storage` ships two `Storage` implementations: `SqliteStorage` (real
runs) and `InMemoryStorage` (used by every core and runtime test). The crate's
three tests exercise **only SQLite**; `InMemoryStorage` has zero direct
coverage. Every Rust test of routing, dispatch, and provisioning therefore
trusts an unverified backend — if the two backends disagree on dispatch-queue
semantics (blocked release, dedup claims, copy recording), core tests pass
while production behaves differently. Plans 005 and 006 modify both backends;
this harness must exist first so those changes are provably parity-preserving.

## Current state

- `crates/wuphf-storage/src/tests.rs` (170 lines) — exactly 3 `#[tokio::test]`
  fns, all constructing `SqliteStorage::connect(...)`:
  - `sqlite_persists_copy_state_and_releases_blocked_reply` (line 5)
  - `sqlite_persists_media_blobs` (line 58)
  - `sqlite_prunes_only_expired_unreferenced_media` (line 88)
- `grep -n InMemory crates/wuphf-storage/src/tests.rs` → no matches.
- The trait: `crates/wuphf-storage/src/lib.rs:45-136` (`pub trait Storage:
  Send + Sync + 'static`, ~30 async methods). Both backends:
  `crates/wuphf-storage/src/memory.rs` (`InMemoryStorage`, RwLock-guarded
  maps), `crates/wuphf-storage/src/sqlite.rs` (`SqliteStorage`).
- Shared semantics helpers already exist so the queue logic stays identical:
  `crates/wuphf-storage/src/shared.rs` (`dispatch_state`, `bind_native_parent`).
- Test helper shape (current file builds fixtures with plain fns):
  `tests.rs:130-169` defines `attachment()`, `message(message_id)`,
  `command(message_id, endpoint_id)` helpers. Keep them.
- Persistence-across-reopen assertions (`drop(storage)` then
  `SqliteStorage::connect` again — `tests.rs:45-54`) are sqlite-specific and
  cannot run against InMemory.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p wuphf-storage` | exit 0, all pass |
| Full gate | `cargo test --workspace` | exit 0 |
| Format / lint | `cargo fmt --check && cargo clippy --workspace --all-targets` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-storage/src/tests.rs`

**Out of scope** (do NOT touch):
- `memory.rs`, `sqlite.rs`, `shared.rs`, `lib.rs` — if a parity test FAILS,
  that is a finding to report, not to fix here (STOP condition).
- Tests in other crates.

## Git workflow

- Branch: `advisor/004-storage-parity-tests`
- Commit style: Conventional Commits; suggested `test: run storage suite against both backends`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Extract backend-generic test bodies

In `tests.rs`, for each of the three existing tests, split the body into a
generic async fn taking `storage: S where S: Storage`, leaving
sqlite-specific reopen assertions in the sqlite wrapper. Pattern:

```rust
async fn copy_state_and_blocked_reply_released<S: Storage>(storage: S) {
    // ... existing body up to (not including) drop(storage)/reopen ...
}

#[tokio::test]
async fn sqlite_persists_copy_state_and_releases_blocked_reply() {
    let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    let storage = SqliteStorage::connect(&url).await.unwrap();
    copy_state_and_blocked_reply_released(storage).await;
    // sqlite-only: reopen and assert pending_dispatches + is_sent_copy persist
    let reopened = SqliteStorage::connect(&url).await.unwrap();
    /* keep existing reopen assertions (tests.rs:47-54) */
}

#[tokio::test]
async fn memory_copy_state_and_blocked_reply_released() {
    copy_state_and_blocked_reply_released(InMemoryStorage::default()).await;
}
```

Problem to solve while extracting: the generic body needs the values the
reopen check asserts against (`released`). Return them from the generic fn
(e.g. `-> Vec<StoredDispatch>`) so the sqlite wrapper can keep its
assertions.

**Verify**: `cargo test -p wuphf-storage` → 6 tests pass (3 sqlite + 3 memory).

### Step 2: Add parity cases for semantics only the core relies on

Add generic bodies (each instantiated for both backends) covering:

1. `claim_event` — first claim of a `DedupKey` returns `true`, second returns
   `false` (dedup is the loop-prevention primitive).
2. `record_sent_copy` + `is_sent_copy` — sent-echo detection; and
   `message_id_for_copy` resolves a received copy.
3. `acknowledge_dispatch` releasing a blocked **edit**: enqueue an edit
   dispatch with `waiting_for = Some(parent)`, acknowledge the parent create,
   assert the released edit's `target_platform_message_id` was bound (uses
   `bind_native_parent` semantics — see `shared.rs:36-43`).
4. `fail_dispatch` — failed dispatch leaves `pending_dispatches()`; a second
   `pending_dispatches()` call does not resurrect it.
5. `replica_set_for_endpoint` — returns the set containing the endpoint, `None`
   for an unknown endpoint. (Plan 005 rewrites the sqlite implementation; this
   test is its safety net.)

Build fixtures with the existing `message`/`command` helpers; follow the
existing test style (no test framework beyond `tokio::test`, plain asserts).

**Verify**: `cargo test -p wuphf-storage` → 16 tests pass (8 generic × 2
backends), exit 0.

### Step 3: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` → exit 0.

## Test plan

This plan *is* tests. New coverage: dedup claim, sent-copy echo, blocked-edit
release binding, fail_dispatch removal from pending, replica-set lookup — each
on both backends. Model after the existing
`sqlite_persists_copy_state_and_releases_blocked_reply` structure.

## Done criteria

- [ ] `cargo test -p wuphf-storage` shows ≥16 passing tests, with each generic body run against both `SqliteStorage` and `InMemoryStorage`
- [ ] `grep -c 'InMemoryStorage' crates/wuphf-storage/src/tests.rs` ≥ 8
- [ ] `cargo test --workspace` exit 0
- [ ] No file outside `crates/wuphf-storage/src/tests.rs` (and `plans/README.md`) modified
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- **Any parity test fails on one backend but passes on the other.** That is a
  real divergence finding — report which method and the differing behavior;
  fixing the backend is out of scope.
- The `Storage` trait gained/lost methods since `15aa85f` (drift; re-check
  which semantics still exist).
- Generic extraction forces a signature change in `memory.rs`/`sqlite.rs`.

## Maintenance notes

- Plans 005 (indexed replica-set lookup) and 006 (metadata-only media check)
  change storage internals; their done criteria re-run this suite. Land this
  plan first.
- Any future `Storage` method should land with a parity test here, not a
  sqlite-only one.
