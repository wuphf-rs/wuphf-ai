# Plan 008: Test the dispatch failure / dead-letter path end to end

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-runtime/src/ crates/wuphf-core/src/acknowledge.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW (test-only)
- **Depends on**: none (land before plans/009, which rewrites provisioning internals)
- **Category**: tests
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

The dead-letter path — a connector reporting `DispatchFailed`, the core
marking the dispatch `Failed`, releasing any provisioning stash, and never
redelivering it — has **zero tests** (`grep -rn fail
crates/wuphf-runtime/src/tests.rs` → no matches; no core test exercises
`fail_dispatch`). This is recovery-critical behavior: a regression here either
redelivers poison dispatches forever or strands stashed messages. Plan 009
rewrites the provisioning stash these paths touch; these characterization
tests must exist first so 009 can prove it preserved behavior.

## Current state

- The failure pipeline:
  - Runtime receives a failure and fans released dispatches back out —
    `crates/wuphf-runtime/src/lib.rs:109-116`:

```rust
                failure = self.failure_rx.recv() => {
                    if let Some((dispatch_id, reason)) = failure {
                        warn!(dispatch_id = %dispatch_id, reason, "dispatch failed permanently, dead-lettering");
                        for dispatch in self.core.fail_dispatch(dispatch_id, reason).await? {
                            self.send(dispatch).await?;
                        }
                    }
                }
```

  - Core dead-letters and releases the provisioning stash —
    `crates/wuphf-core/src/acknowledge.rs:170-184` (`fail_dispatch` calls
    `resume_provisioned`, which replays stashed messages once no provisioning
    dispatch remains pending).
  - Storage marks the state — `SqliteStorage::fail_dispatch`
    (`crates/wuphf-storage/src/sqlite.rs:531-`), `DispatchState::Failed`
    (`crates/wuphf-model/src/dispatch.rs:115-117`); failed dispatches must not
    appear in `pending_dispatches()` (whose query is
    `SELECT ... WHERE state = 'pending' ORDER BY rowid`, sqlite.rs:430).
  - Hub-level authorization: only the session a dispatch was assigned to may
    report failure — `crates/wuphf-runtime/src/connector.rs:196-204`.
- Existing test exemplars in `crates/wuphf-runtime/src/tests.rs`:
  - channel-level: `acknowledgement_releases_reply_with_target_native_parent_id`
    (line 108) — drive `channels.events` / `channels.acknowledgements`, read
    `channels.dispatches`; `RuntimeChannels` also exposes `failures`
    (`crates/wuphf-runtime/src/lib.rs:35`).
  - hub-level: `connector_sessions_route_dispatches_and_authorize_inputs`
    (line 144) — `ConnectorHub::new`, `handle.connect(...)`, sessions send
    `ConnectorInput::...`, read `ConnectorOutput::...`. `ConnectorInput::DispatchFailed`
    exists (`crates/wuphf-model/src/connector.rs:89-92`).
  - mirror/provisioning fixtures: `mirror_rule()` provisioning flow is shown in
    core tests (`concurrent_messages_provision_one_thread_and_replay_the_rest`,
    `crates/wuphf-core/src/tests.rs:289`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Runtime tests | `cargo test -p wuphf-runtime` | exit 0 |
| Core tests | `cargo test -p wuphf-core` | exit 0 |
| Full gate | `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-runtime/src/tests.rs`
- `crates/wuphf-core/src/tests.rs` (one stash-release test)

**Out of scope** (do NOT touch):
- Any non-test source. A failing expectation = STOP and report.

## Git workflow

- Branch: `advisor/008-failure-path-tests`
- Commit style: Conventional Commits; suggested `test: cover dispatch dead-lettering and stash release on failure`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Runtime channel-level test — failed dispatch is not redelivered

In `crates/wuphf-runtime/src/tests.rs`, model after
`acknowledgement_releases_reply_with_target_native_parent_id` (line 108):

`failed_dispatch_is_dead_lettered_and_not_redelivered`:
1. ingest `wa_event("wa-1", None)`, receive the dispatch;
2. send `(dispatch.id, "invalid target".to_string())` on `channels.failures`;
3. assert no dispatch arrives (use
   `tokio::time::timeout(Duration::from_millis(100), channels.dispatches.recv())`
   → `Err(Elapsed)`);
4. restart simulation: build a **new** `Runtime` over the same storage —
   note `core()` (tests.rs:89) hides storage construction; lift it so the test
   keeps an `Arc<InMemoryStorage>` and builds `Core::new` directly (see
   `core()` body, one line) — then assert the redelivered pending set is
   empty (`channels.dispatches` yields nothing within the timeout).

**Verify**: `cargo test -p wuphf-runtime` → new test passes.

### Step 2: Hub-level test — only the assigned session may fail a dispatch

Model after `connector_sessions_route_dispatches_and_authorize_inputs`
(line 144): whatsapp session sends an event; telegram receives the dispatch;
**whatsapp** (not telegram) sends
`ConnectorInput::DispatchFailed { dispatch_id, reason }` → whatsapp receives
`ConnectorOutput::Rejected { .. }`; then telegram sends the same
`DispatchFailed` → no rejection, and the dispatch is dead-lettered (no
redelivery to a fresh telegram session after reconnect).

**Verify**: `cargo test -p wuphf-runtime` → passes.

### Step 3: Core-level test — failed provisioning dispatch releases the stash

In `crates/wuphf-core/src/tests.rs`, model after
`concurrent_messages_provision_one_thread_and_replay_the_rest` (line 289)
which builds the mirror-rule provisioning + stash state:

`failed_provisioning_releases_stashed_messages`:
1. mirror rule; ingest message A → provisioning dispatch (carries
   `provision_thread`); ingest message B → `IngestOutcome::Deferred` (stashed);
2. call `core.fail_dispatch(provisioning_dispatch_id, "cannot create topic".into())`;
3. assert the returned dispatches replay message B (a fresh create dispatch
   carrying a new `provision_thread` — per the comment at
   `acknowledge.rs:177-179`, replay re-attempts provisioning);
4. assert the failed dispatch is absent from `core.pending_dispatches()`.

**Verify**: `cargo test -p wuphf-core` → passes.

### Step 4: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` → exit 0.

## Test plan

The three tests above are the plan. They are *characterization* tests:
assert what the code does today as documented in "Current state".

## Done criteria

- [ ] `grep -c 'fail' crates/wuphf-runtime/src/tests.rs` ≥ 2
- [ ] `cargo test -p wuphf-runtime` and `cargo test -p wuphf-core` exit 0 with 2 and 1 new tests respectively
- [ ] `cargo test --workspace` exit 0; only test files modified
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Step 3 reveals the replay does NOT re-attempt provisioning (returned
  dispatches empty or missing `provision_thread`) — possible real bug; report
  the observed outcome verbatim.
- `RuntimeChannels.failures` no longer exists (drift).
- Hub test cannot observe dead-lettering without sleeping >1s (signals a
  flaky design; report rather than land a flaky test).

## Maintenance notes

- Plan 009 (provisioning stash durability) changes `resume_provisioned`
  internals; these tests are its behavioral contract. If 009 intentionally
  changes a behavior pinned here (e.g. replay semantics after restart), it
  must update these tests explicitly in its diff.
- Reviewer: watch for time-based flakiness; the 100ms timeouts must only
  guard "nothing arrives", never "something arrives".
