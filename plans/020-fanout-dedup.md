# Plan 020: Deduplicate the per-operation fan-out skeleton in router and core

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 52c5084..HEAD -- crates/wuphf-router crates/wuphf-core`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: tech-debt
- **Planned at**: commit `52c5084`, 2026-07-07

## Why this matters

Every replicated operation that acts on an existing message copy — edit,
delete, reaction, delivery/read receipt — re-implements the same ~50-line
fan-out loop in the router and the same ~15-line "enqueue routing outcome"
block in the core. When receipts were added recently, the skeleton was copied
twice more; the next operation (e.g. executing reactions natively, typing
indicators) will copy it again. Worse, a fix to the blocking semantics (how a
target whose copy isn't delivered yet gets parked) currently has to be applied
in four places in the router and five in the core, and a missed one is a silent
behavioral divergence between operations. This plan collapses the copies into
one helper per crate. It is a behavior-neutral refactor protected by the
existing 24 core tests, which cover every operation's delivered and blocked
paths.

## Current state

- `crates/wuphf-router/src/routing.rs` — `route_edit` (lines 100–154),
  `route_delete` (156–210), `route_reaction` (216–272), `route_receipt`
  (282–340) all share this exact skeleton, differing **only** in the payload
  constructed in the two match arms:

```rust
let replica_set = self.storage.replica_set(message.replica_set_id).await?;
if !replica_set.endpoints.contains(&origin_endpoint) {
    return Err(RouterError::OriginOutsideReplicaSet(origin_endpoint));
}
let mut outcome = RoutingOutcome::default();
for endpoint_id in replica_set.endpoints.into_iter()
    .filter(|endpoint_id| *endpoint_id != origin_endpoint)
{
    let endpoint = self.storage.endpoint(endpoint_id).await?;
    let target = TargetAddress { address: endpoint.address(), provision_thread: None };
    let target_connector = endpoint.connector_id.clone();
    match self.storage.copy_at_endpoint(message.id, endpoint_id).await? {
        Some(copy) => outcome.dispatches.push(DispatchCommand {
            target_endpoint: endpoint_id, target_connector, target,
            payload: /* per-op payload with copy.platform_message_id */,
        }),
        None => outcome.blocked.push(BlockedDispatch {
            command: DispatchCommand { /* same, payload with String::new() */ },
            waiting_for_message: message.id,
        }),
    }
}
Ok(outcome)
```

  Per-operation payloads: edit needs `self.config.delivery(&target_connector).edit_mode`
  captured *inside* the loop (it depends on the target connector); delete
  likewise with `.delete_mode`; reaction and receipt already build payloads via
  a local closure over the empty-or-native id (`routing.rs:241–248`, `305–316`).
  `route` (create, lines 17–93) has a genuinely different shape (reply-parent
  blocking, thread provisioning, delivery options) — it is **not** part of the
  consolidation.

- `crates/wuphf-core/src/ingest.rs` — `ingest_edit` (218–257), `ingest_delete`
  (263–301), `ingest_reaction` (309–348), `ingest_receipt` (361–402), and
  `maybe_retitle` (139–171) each end with a copy of:

```rust
let mut dispatches = Vec::new();
for command in routing.dispatches.iter().cloned() {
    dispatches.push(self.storage.enqueue_dispatch(command, None).await?);
}
for blocked in routing.blocked.iter().cloned() {
    self.storage
        .enqueue_dispatch(blocked.command, Some(blocked.waiting_for_message))
        .await?;
}
```

  (`maybe_retitle` has only the first loop — retitles are never blocked.)
  `route_and_record` (178–212) has this too, but interleaved with
  provisioning-id bookkeeping — leave it alone.

  The four `ingest_*` functions also share a resolve prolog
  (`message_id_for_copy` → `Ignored(Unmatched)` on miss → `storage.message` →
  per-op `CoreError::*TargetNotStored`); consolidating it is optional Step 4.

- `crates/wuphf-router/src/lib.rs` — `RoutingOutcome { dispatches, blocked }`
  and `BlockedDispatch { command, waiting_for_message }` (lines 126–136).
- `crates/wuphf-core/src/tests.rs` — the regression net. Relevant tests:
  `edit_after_delivery_revises_the_delivered_copy` (573),
  `edit_before_delivery_blocks_then_releases_with_native_id` (619),
  `reaction_after_delivery_fans_to_the_delivered_copy` (723),
  `reaction_before_delivery_blocks_then_releases_with_native_id` (770),
  `read_receipt_on_a_mirror_copy_surfaces_a_seen_receipt_on_the_origin` (864),
  plus delete and ignore-path tests.

Conventions: private helpers live in the same file below the public methods;
doc comments state *why* (see the header comments on `route_receipt`,
`routing.rs:274–281` — keep them on the public wrappers, not the helper).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full test suite | `cargo test --workspace` | exit 0; 52+ tests pass |
| Router compile | `cargo build -p wuphf-router` | exit 0 |
| Core tests | `cargo test -p wuphf-core` | exit 0; 24 tests pass |

Note: the build needs `protoc` on PATH (`brew install protobuf` on macOS).

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-router/src/routing.rs`
- `crates/wuphf-core/src/ingest.rs`

**Out of scope** (do NOT touch, even though they look related):
- `Router::route` (the create path) — different semantics (reply blocking,
  provisioning); forcing it through the shared helper would obscure both.
- `Core::route_and_record` and `crates/wuphf-core/src/acknowledge.rs` —
  provisioning bookkeeping is interleaved there; plan 009 rewrites that area.
- `crates/wuphf-router/src/lib.rs`, `crates/wuphf-model/**`,
  `crates/wuphf-storage/**` — no type or trait changes.
- Public signatures of `route_edit`/`route_delete`/`route_reaction`/
  `route_receipt` and all `ingest_*` — callers and tests must not change.

## Git workflow

- Branch: `advisor/020-fanout-dedup` off `v2`.
- Commits: `refactor: share the copy-targeting fan-out loop across router
  operations`, then `refactor: share dispatch enqueueing in core ingest`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Extract the router fan-out helper

In `crates/wuphf-router/src/routing.rs`, add a private method:

```rust
/// Fan an operation on an existing message out to every other endpoint in
/// the replica set. Each target either carries the native id of its copy or
/// is blocked on the message until that copy is delivered. `payload_for`
/// builds the per-target payload from (target connector, native id) — the
/// native id is empty while blocked and bound on release.
async fn fan_out_to_copies(
    &self,
    message: &Message,
    origin_endpoint: EndpointId,
    payload_for: impl Fn(&ConnectorId, String) -> DispatchPayload,
) -> Result<RoutingOutcome, RouterError>
```

Body = the shared skeleton from "Current state". Passing the target
`ConnectorId` into the closure lets edit/delete resolve their per-target
`edit_mode`/`delete_mode` via `self.config.delivery(connector)` — capture
`&self.config` in the closure at the call site.

**Verify**: `cargo build -p wuphf-router` → exit 0.

### Step 2: Rewire the four route methods

`route_edit`, `route_delete`, `route_reaction`, `route_receipt` become
wrappers that keep their signatures and doc comments and delegate:

```rust
pub async fn route_edit(&self, message: &Message, origin_endpoint: EndpointId)
    -> Result<RoutingOutcome, RouterError>
{
    self.fan_out_to_copies(message, origin_endpoint, |connector, native_id| {
        DispatchPayload::MessageEdit {
            message: message.clone(),
            target_platform_message_id: native_id,
            mode: self.config.delivery(connector).edit_mode,
        }
    })
    .await
}
```

(delete/reaction/receipt analogous; reaction clones `reactor`/`reactor_name`/
`emoji` into the closure, receipt matches on `kind` as today.)

**Verify**: `cargo test --workspace` → exit 0, same count as baseline. Then
`grep -c "OriginOutsideReplicaSet" crates/wuphf-router/src/routing.rs` → 2
(once in `route`, once in the helper).

### Step 3: Extract the core enqueue helper

In `crates/wuphf-core/src/ingest.rs`, add:

```rust
/// Persist a routing outcome: enqueue the sendable dispatches and park the
/// blocked ones on the message they wait for. Returns only the sendable
/// dispatches — blocked ones surface later, when their parent is acknowledged.
async fn enqueue_outcome(
    &self,
    routing: &RoutingOutcome,
) -> Result<Vec<StoredDispatch>, CoreError>
```

Replace the duplicated block in `ingest_edit`, `ingest_delete`,
`ingest_reaction`, `ingest_receipt`, and `maybe_retitle` (whose `blocked` list
is always empty, so the shared helper is safe there) with a call to it. Do NOT
touch `route_and_record`.

**Verify**: `cargo test -p wuphf-core` → 24 pass. Then
`grep -c "enqueue_dispatch" crates/wuphf-core/src/ingest.rs` → 3 (helper's two
call sites + `route_and_record`'s loop… count the actual occurrences before
and assert the number dropped from 11 to 4; adjust the expected count to what
you observe, the point is: outside `enqueue_outcome` and `route_and_record`
there are none).

### Step 4 (optional, skip if any friction): Share the resolve prolog

Add `async fn message_for_copy(&self, endpoint: EndpointId, platform_message_id: &str, missing: fn(MessageId) -> CoreError) -> Result<Option<(MessageId, Message)>, CoreError>`
and use it in the four `ingest_*` functions, passing the operation's
`CoreError::EditTargetNotStored` / `DeleteTargetNotStored` /
`ReactionTargetNotStored` / `ReceiptTargetNotStored` constructor. Keep the
`Ignored(Unmatched)` short-circuit at each call site.

**Verify**: `cargo test -p wuphf-core` → 24 pass.

## Test plan

No new tests: this is a behavior-neutral refactor and the existing
`wuphf-core/src/tests.rs` suite already covers each operation's delivered path,
blocked-then-released path, and ignore paths (test names listed in "Current
state"). The done criterion is the full suite passing with an unchanged count.
If you find an operation/path pair *not* covered (e.g. delete-before-delivery
blocking), add one test modeled on
`edit_before_delivery_blocks_then_releases_with_native_id`
(`crates/wuphf-core/src/tests.rs:619`) rather than proceeding uncovered.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `cargo test --workspace` exits 0 with at least the baseline 52 tests passing
- [ ] `route_edit`, `route_delete`, `route_reaction`, `route_receipt` each delegate to `fan_out_to_copies` (each body ≤ ~15 lines)
- [ ] The five duplicated enqueue blocks in `ingest.rs` are gone; `enqueue_outcome` exists
- [ ] Public signatures unchanged: `git diff` shows no edits to `crates/wuphf-router/src/lib.rs` or any file outside the in-scope list
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The four route methods' loops turn out NOT to be identical in some detail
  the excerpts above miss (e.g. one filters endpoints differently) — that
  difference may be load-bearing; report it instead of normalizing it.
- Lifetime/borrow issues force changing a public signature or adding
  allocation beyond the `message.clone()` per target that already happens
  today.
- Any existing test fails after a step and the fix isn't an obvious mechanical
  slip in the refactor.

## Maintenance notes

- The next replicated operation should be one `DispatchPayload` variant, one
  `payload_for` closure in a thin `route_*` wrapper, and one `ingest_*` using
  `message_for_copy` + `enqueue_outcome`. If someone finds themselves copying
  a loop again, the helper's contract is too narrow — widen it instead.
- Reviewer should scrutinize: `edit_mode`/`delete_mode` are still resolved
  per *target* connector (inside the closure), not hoisted to the origin's.
- Deferred: unifying `route` (create) and `route_and_record` — different
  semantics; and plan 009 owns the provisioning-stash region.
