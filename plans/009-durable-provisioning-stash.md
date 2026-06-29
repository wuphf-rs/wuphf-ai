# Plan 009: Make the provisioning stash durable so a crash cannot lose deferred messages

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-core/src/ crates/wuphf-storage/src/ crates/wuphf-runtime/src/lib.rs migrations/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1 (highest-impact correctness finding of the audit)
- **Effort**: M
- **Risk**: MED (touches ingest/acknowledge/fail paths; mitigated by plans 007/008 landing first)
- **Depends on**: plans/008-dispatch-failure-path-tests.md (characterization of fail/resume), plans/004-storage-backend-parity-tests.md (storage test harness)
- **Category**: bug
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

When a conversation's mirror thread is being provisioned, messages that arrive
in the meantime are stashed **in memory only**
(`Core.provisioning: Mutex<HashMap<ReplicaSetId, ProvisionState>>`). Two
failure modes follow:

1. **Crash = silent message loss.** A stashed message was already persisted
   via `put_message`, but it has no dispatches. Restart recovery
   (`Runtime::run`, `crates/wuphf-runtime/src/lib.rs:69-78`) only redelivers
   *dispatches*; the stashed message is never routed, never delivered, and
   nothing ever reports it. Worse, the in-memory pending set is also gone, so
   when the redelivered provisioning dispatch is acknowledged,
   `resume_provisioned` finds no state and replays nothing.
2. **Unbounded RAM growth.** If provisioning never settles (connector down
   for days), every message to that conversation accumulates in the in-process
   `Vec<StashedMessage>`.

The fix: keep the pending-dispatch set rebuildable from storage (it already
is — pending dispatches carrying `provision_thread` are persisted) and move
the stash itself into a storage table, so both restart recovery and memory use
are sound.

## Current state

- The in-memory state, `crates/wuphf-core/src/lib.rs:20-32`:

```rust
    /// Conversations whose mirror thread is mid-provisioning, keyed by replica
    /// set. Guards against a burst of messages each provisioning a duplicate
    /// thread.
    provisioning: Mutex<HashMap<ReplicaSetId, ProvisionState>>,
```

  with `ProvisionState { pending: <set of DispatchId>, stash: Vec<StashedMessage> }`
  and `StashedMessage { message, origin_endpoint, provision_title }` (read the
  exact definitions near the top of `crates/wuphf-core/src/lib.rs` /
  `ingest.rs`; `StashedMessage` fields are visible at
  `crates/wuphf-core/src/ingest.rs:98-102`).

- Stashing on ingest, `crates/wuphf-core/src/ingest.rs:94-112`:

```rust
        let stashed = {
            let mut provisioning = self.provisioning.lock().unwrap();
            match provisioning.get_mut(&replica_set_id) {
                Some(state) if !state.pending.is_empty() => {
                    state.stash.push(StashedMessage {
                        message: message.clone(),
                        origin_endpoint,
                        provision_title: provision_title.clone(),
                    });
                    true
                }
                _ => false,
            }
        };
        if stashed {
            return Ok(IngestOutcome::Deferred { message_id: message.id });
        }
```

- Pending-set registration after routing, `ingest.rs:203-210`
  (`route_and_record`): dispatch ids whose command has
  `target.provision_thread.is_some()` are added to
  `provisioning[replica_set].pending`.

- Drain on settle (ack or fail), `crates/wuphf-core/src/acknowledge.rs:59-90`
  (`resume_provisioned`): removes the dispatch id from `pending`; when
  `pending` empties, takes the stash and re-routes every stashed message via
  `route_and_record`.

- Restart recovery redelivers only dispatches,
  `crates/wuphf-runtime/src/lib.rs:69-78`.

- Storage conventions for the new table: JSON-payload tables + sequential
  migrations (`migrations/0001..0007`; plan 005 may have taken `0008` — use
  the next free number). Transaction pattern: `put_alias`
  (`crates/wuphf-storage/src/sqlite.rs:90-`).

- Characterization tests that pin today's behavior (must keep passing, with
  the one restart-behavior change called out below):
  `concurrent_messages_provision_one_thread_and_replay_the_rest`
  (`crates/wuphf-core/src/tests.rs:289`), plan 008's
  `failed_provisioning_releases_stashed_messages`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| Core tests | `cargo test -p wuphf-core` | exit 0 |
| Runtime tests | `cargo test -p wuphf-runtime` | exit 0 |
| Full gate | `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` | exit 0 |

## Scope

**In scope** (the only files you should create/modify):
- `migrations/000N_provisioning_stash.sql` (create; N = next free)
- `crates/wuphf-storage/src/lib.rs` (3 new trait methods), `sqlite.rs`,
  `memory.rs`, `tests.rs`
- `crates/wuphf-core/src/lib.rs`, `ingest.rs`, `acknowledge.rs`
- `crates/wuphf-runtime/src/lib.rs` (startup rebuild call), `tests.rs`
- `crates/wuphf-core/src/tests.rs` (new restart test)

**Out of scope** (do NOT touch):
- The dispatch state machine / `dispatches` table schema.
- Provisioning **timeouts** (auto-failing a hung provision) — deliberate
  deferral, see Maintenance notes.
- `crates/wuphf-runtime/src/connector.rs` (hub redelivery is untouched).

## Git workflow

- Branch: `advisor/009-durable-provisioning-stash`
- Commit per step; Conventional Commits. Suggested final squash message:
  `fix: persist the provisioning stash so restarts cannot lose deferred messages`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Storage — stash table and trait methods

Migration (next free number), one row per stashed message, insertion-ordered:

```sql
CREATE TABLE provisioning_stash (
    replica_set_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    origin_endpoint TEXT NOT NULL,
    provision_title TEXT NOT NULL,
    PRIMARY KEY (replica_set_id, message_id)
);
```

Trait methods (`crates/wuphf-storage/src/lib.rs`), implemented in both
backends:

```rust
    /// Persist one provisioning-deferred message for later replay.
    async fn stash_provisioning_message(
        &self,
        replica_set_id: ReplicaSetId,
        message_id: MessageId,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> StorageResult<()>;
    /// Remove and return the stashed messages for a replica set, in insertion
    /// order, resolved back to full messages.
    async fn drain_provisioning_stash(
        &self,
        replica_set_id: ReplicaSetId,
    ) -> StorageResult<Vec<(Message, EndpointId, String)>>;
    /// Replica sets that still have stashed messages (startup recovery).
    async fn stashed_replica_sets(&self) -> StorageResult<Vec<ReplicaSetId>>;
```

SQLite `drain` joins `messages` on `message_id`, orders by `rowid`, and
deletes the drained rows in the same transaction. InMemory mirrors with a
`HashMap<ReplicaSetId, Vec<...>>` under the existing `RwLock` state struct.

Parity tests (plan 004 style, both backends): stash two → drain returns both
in order and second drain returns empty; `stashed_replica_sets` lists the set
before drain and not after.

**Verify**: `cargo test -p wuphf-storage` → exit 0.

### Step 2: Core — stash to storage instead of memory

- `ProvisionState` loses its `stash: Vec<StashedMessage>` field; it keeps
  `pending` (the in-memory mutex map remains, as a cache of pending dispatch
  ids — it is rebuildable, see Step 4).
- `ingest.rs:94-112`: where the code currently pushes onto `state.stash`,
  instead (a) check `pending.is_empty()` under the lock as today, (b) **after
  releasing the lock**, call `storage.stash_provisioning_message(...)`, then
  return `Deferred`. (The lock must not be held across the await — current
  code already scopes it; keep that shape.)
  - Race note: between the lock check and the storage write, the provisioning
    could settle. Handle it the same way the current code would handle a
    settle racing a stash push: after the storage write, re-check
    `provisioning` under the lock; if the entry is gone (settled meanwhile),
    drain-and-replay immediately (call the same replay helper as
    `resume_provisioned`) so the message is not stranded.
- `acknowledge.rs` `resume_provisioned` (lines 59-90): when `pending` empties,
  call `storage.drain_provisioning_stash(replica_set_id)` and replay each
  entry through `route_and_record` exactly as the current stash loop does
  (lines 78-88). Reconstruct `StashedMessage`-equivalent data from the tuple.
- Remove the now-unused `StashedMessage` struct if nothing else references it.

**Verify**: `cargo test -p wuphf-core` → all pass, including
`concurrent_messages_provision_one_thread_and_replay_the_rest` (behavioral
contract: stash + replay semantics unchanged for the no-crash path).

### Step 3: Restart recovery — rebuild pending set, replay orphaned stashes

In `Core` add:

```rust
    /// Rebuild the provisioning cache from storage after a restart: every
    /// pending dispatch that provisions a thread re-registers its replica
    /// set; any stashed replica set with NO such dispatch left (its
    /// provision was acknowledged or failed in a previous run, or was lost)
    /// is drained and replayed immediately.
    pub async fn recover_provisioning(&self) -> Result<Vec<StoredDispatch>, CoreError>
```

Implementation: (a) `pending_dispatches()`; for each with
`command.target.provision_thread.is_some()`, insert into
`provisioning[command.message_replica_set_id()].pending`; (b)
`storage.stashed_replica_sets()`; for each with no rebuilt pending entry,
drain + replay via the same helper as `resume_provisioned`, collecting the
resulting dispatches.

In `Runtime::run` (`crates/wuphf-runtime/src/lib.rs:68-78`), call
`self.core.recover_provisioning().await?` **before** the
`pending_dispatches()` redelivery loop, and send any dispatches it returns
after the redelivered ones.

**Verify**: `cargo test -p wuphf-runtime` → exit 0.

### Step 4: New tests — the crash scenarios this plan exists for

Core test (`crates/wuphf-core/src/tests.rs`),
`restart_replays_stash_when_provisioning_settled_while_down`:
1. mirror rule; ingest A (provisioning dispatch), ingest B (Deferred);
2. build a **second** `Core` over the same `Arc<InMemoryStorage>` (simulated
   restart — the mutex map starts empty);
3. acknowledge A's provisioning dispatch on the new core with a thread id →
   assert the returned dispatches replay B into the provisioned thread;
   (this passes only because `recover_provisioning` re-registered the pending
   set — call it in the test after constructing the new core, mirroring what
   `Runtime::run` does).

Runtime test (`crates/wuphf-runtime/src/tests.rs`),
`restart_with_orphaned_stash_replays_immediately`: stash B as above, then
fail A's provisioning dispatch, **then** restart (new Runtime over same
storage) without any further acks → B's replacement dispatches appear on
`channels.dispatches` at startup. (Covers the lost-settle case: stash rows
with no pending provision left.)

**Verify**: `cargo test --workspace` → exit 0.

## Test plan

- Parity tests (Step 1), behavioral contract tests already in tree
  (plans 007/008 + `concurrent_messages_...`), and the two restart tests
  (Step 4) — the second one MUST fail if written against the pre-plan code;
  run it once against unmodified `ingest.rs` to confirm it reproduces the
  message loss, then apply the fix.
- Full gate: `cargo fmt --check && cargo clippy --workspace --all-targets &&
  cargo test --workspace` → exit 0.

## Done criteria

- [ ] `grep -n 'stash: Vec' crates/wuphf-core/src/lib.rs crates/wuphf-core/src/ingest.rs` → no matches (stash no longer in memory)
- [ ] `grep -n 'recover_provisioning' crates/wuphf-runtime/src/lib.rs` → called in `run()` before redelivery
- [ ] Both Step-4 tests pass; `cargo test --workspace` exit 0
- [ ] Parity tests cover the three new storage methods on both backends
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Plans 004 and 008 are not landed (their tests are this plan's safety net).
- The re-check race in Step 2 cannot be closed without holding the mutex
  across an await — report the design problem instead of doing that (deadlock
  risk with the runtime's single-threaded select loop).
- `concurrent_messages_provision_one_thread_and_replay_the_rest` fails after
  Step 2 in a way that suggests replay ordering changed (stash must replay in
  insertion order).
- Any storage method signature change beyond the three additions becomes
  necessary.

## Maintenance notes

- Deliberately deferred: a **provisioning timeout** (auto-fail a provision no
  connector has acked for N minutes). With durability, a hung provision now
  strands messages on disk instead of RAM until the connector returns —
  visible and recoverable, but still stranded. If operators hit this, add a
  timeout that calls `fail_dispatch` on overdue provisioning dispatches.
- The in-memory `provisioning` map is now a rebuildable cache; any new code
  path that mutates it must keep storage authoritative (write storage first).
- Reviewer: scrutinize the Step-2 race re-check and that no `.lock().unwrap()`
  is held across `.await` (existing convention, `ingest.rs:94-107`).
