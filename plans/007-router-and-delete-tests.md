# Plan 007: Test delete replication and the router's untested branches

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-router/src/ crates/wuphf-core/src/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW (test-only)
- **Depends on**: none
- **Category**: tests
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

Message-**delete** replication has zero test coverage anywhere in the
workspace: `crates/wuphf-core/src/tests.rs` has 17 tests covering creates,
edits, provisioning, replies, dedup, echoes, and migration — none named or
exercising delete. `Core::ingest_delete` (`crates/wuphf-core/src/ingest.rs:263-301`)
and `Router::route_delete` (`crates/wuphf-router/src/routing.rs:156-210`) ship
untested, including the delete echo-loop breaker (`message.deleted` redundant-
delete check) that prevents infinite delete ping-pong between platforms.
Separately, the `wuphf-router` crate has no unit tests of its own; its error
branches (`OriginOutsideReplicaSet`, reply-parent `MessageNotFound`) and
retitle skip conditions are reachable only through narrow core paths and are
currently unpinned. Routing is the highest-churn area of the repo; these tests
are the regression net for plans that touch routing later.

## Current state

- `crates/wuphf-core/src/tests.rs` — 702 lines, 17 `#[tokio::test]`s. Existing
  helpers to reuse (do not reinvent):
  - `connectors()` (line 13), `direct_rule()` (37), `mirror_rule()` (59),
    `core(rules: Vec<Rule>)` (80) → `Core<InMemoryStorage>`,
    `event(...)` (86) / `event_in(...)` (95), `edit_event(...)` (541).
  - Test names follow behavior phrasing:
    `edit_after_delivery_revises_the_delivered_copy`.
- Delete model semantics (what the tests must pin), from
  `crates/wuphf-core/src/ingest.rs:259-301`:
  - delete of an unknown copy → `IngestOutcome::Ignored(IgnoreReason::Unmatched)`;
  - delete of an already-deleted message →
    `Ignored(IgnoreReason::RedundantDelete)` (the echo-loop breaker;
    `message.deleted` survives because copies are retained — see comment in
    `crates/wuphf-model/src/message.rs:56-60`);
  - otherwise: message marked `deleted = true`, persisted, fanned out via
    `route_delete`; targets with a copy get
    `DispatchPayload::MessageDelete { target_platform_message_id, mode }`,
    targets without one get a **blocked** dispatch waiting on the message
    (`routing.rs:194-206`) with empty `target_platform_message_id`, bound on
    release (`bind_native_parent`, `crates/wuphf-storage/src/shared.rs:36-43`).
  - `mode` comes from the target connector's `DeliveryOptions.delete_mode`
    (default `Revoke`; `crates/wuphf-model/src/connector.rs:32-38`).
- Router branches with no direct tests, `crates/wuphf-router/src/`:
  - `route`/`route_edit`/`route_delete` return
    `RouterError::OriginOutsideReplicaSet` when the origin endpoint is not in
    the replica set (`routing.rs:24-26,106-108,162-164`);
  - `route` returns `RouterError::MessageNotFound` when a reply parent's copy
    exists but the parent message is gone (`routing.rs:53-57`);
  - `route_retitle` skips endpoints that are not mirror sides and mirror
    endpoints with no provisioned thread (`routing.rs:229-240`), and returns
    empty when the rule id is unknown (`routing.rs:225-227`);
  - `side_accepts` selector logic (`lib.rs:81-92`): `Single` thread match,
    `Source` include/exclude, `Mirror` container match.
- Router has no `#[cfg(test)]` module and no `tests.rs`; `wuphf-storage`
  declares its test module as `#[cfg(test)] mod tests;` at the bottom of
  `lib.rs` (`crates/wuphf-storage/src/lib.rs:138-139`) — follow that pattern.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Router tests | `cargo test -p wuphf-router` | exit 0, new tests pass |
| Core tests | `cargo test -p wuphf-core` | exit 0, 17 + new pass |
| Full gate | `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` | exit 0 |

## Scope

**In scope** (the only files you should create/modify):
- `crates/wuphf-core/src/tests.rs` — delete-path tests
- `crates/wuphf-router/src/tests.rs` (create) + the `#[cfg(test)] mod tests;`
  line in `crates/wuphf-router/src/lib.rs`
- `crates/wuphf-router/Cargo.toml` — only if dev-dependencies are needed
  (`tokio` with `macros`, `wuphf-storage` is already a dependency)

**Out of scope** (do NOT touch):
- Any non-test source code. If a test reveals a bug, STOP and report.
- `crates/wuphf-runtime/src/tests.rs` (plan 008 owns runtime failure tests).

## Git workflow

- Branch: `advisor/007-router-delete-tests`
- Commit style: Conventional Commits; suggested `test: cover delete replication and router branches`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Delete-path tests in `wuphf-core`

Add to `crates/wuphf-core/src/tests.rs`, reusing `core(...)`, `direct_rule()`,
`event(...)` helpers and following the edit tests
(`edit_after_delivery_revises_the_delivered_copy`, line 562) as the structural
pattern — they do exactly this dance for edits. You will need a small
`delete_event(...)` helper mirroring `edit_event(...)` (line 541) that builds
`EventPayload::MessageDelete { platform_message_id }`.

Tests:

1. `delete_after_delivery_revokes_the_delivered_copy` — ingest create, ack the
   dispatch (record the copy), ingest delete → one
   `DispatchPayload::MessageDelete` dispatch with the copy's
   `target_platform_message_id` and default mode `DeleteMode::Revoke`; stored
   message has `deleted == true`.
2. `delete_before_delivery_blocks_then_releases_with_native_id` — ingest
   create, do NOT ack, ingest delete → no immediate delete dispatch; ack the
   create → released delete dispatch carries the ack'd platform message id
   (mirror of `edit_before_delivery_blocks_then_releases_with_native_id`,
   line 608).
3. `redundant_delete_is_ignored` — ingest delete twice; second returns
   `IngestOutcome::Ignored(IgnoreReason::RedundantDelete)` and produces no
   dispatches (the echo-loop breaker).
4. `delete_for_unknown_message_is_ignored` — delete with an unseen
   `platform_message_id` → `Ignored(Unmatched)`.

**Verify**: `cargo test -p wuphf-core` → 21 tests pass.

### Step 2: Router unit tests

Create `crates/wuphf-router/src/tests.rs` and register it in `lib.rs` with
`#[cfg(test)] mod tests;` (match `wuphf-storage`'s placement). Build fixtures
directly against `InMemoryStorage`: `put_endpoint`, `put_replica_set`,
`put_message`, `put_received_copy`, `put_conversation`, `put_thread_mapping`
(see trait, `crates/wuphf-storage/src/lib.rs:45-136`). Construct
`Router::new(storage, config)` with a `RoutingConfig` built like
`crates/wuphf-runtime/src/tests.rs:24-72` builds rules/connector-infos.

Tests:

1. `route_rejects_origin_outside_replica_set` — replica set without the origin
   endpoint → `Err(RouterError::OriginOutsideReplicaSet)`; same assertion via
   `route_edit` and `route_delete`.
2. `route_blocks_reply_when_parent_copy_missing` — message with
   `reply_to: Some(parent)` and no copy at the target →
   `outcome.blocked` has one entry waiting on the parent, `outcome.dispatches`
   empty.
3. `route_errors_when_reply_parent_message_is_gone` — copy exists
   (`put_received_copy`) but parent message never stored →
   `Err(RouterError::MessageNotFound)`.
4. `route_retitle_skips_unprovisioned_and_non_mirror_endpoints` — rule with one
   source + one mirror side; no thread mapping → empty outcome; add a thread
   mapping → exactly one `RetitleThread` dispatch targeting that thread.
5. `side_accepts_selectors` (plain `#[test]`) — table-driven over
   `side_accepts`: `Single` with/without thread match, `Source` empty include
   (= all) / include hit / exclude hit, `Mirror` container match/mismatch.

**Verify**: `cargo test -p wuphf-router` → ≥5 tests pass.

### Step 3: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` → exit 0.

## Test plan

This plan is tests; the list above is the plan. Patterns: edit tests in
`wuphf-core/src/tests.rs:541-700`, rule fixtures in
`wuphf-runtime/src/tests.rs:24-72`.

## Done criteria

- [ ] `cargo test -p wuphf-core` ≥ 21 passing (17 baseline + 4 delete)
- [ ] `cargo test -p wuphf-router` ≥ 5 passing
- [ ] `grep -c 'async fn delete_\|fn delete_' crates/wuphf-core/src/tests.rs` ≥ 2
- [ ] `cargo test --workspace` exit 0; no non-test file modified
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Any new test fails because the production behavior differs from the
  semantics described in "Current state" (e.g. a redundant delete routes
  dispatches anyway). That is a bug finding — report with the failing output;
  do not change source.
- The helpers in `wuphf-core/src/tests.rs` were refactored away (drift).
- `route_retitle` test cannot reach the no-rule branch without constructing an
  inconsistent conversation — acceptable to skip that one assertion; note it.

## Maintenance notes

- These tests pin today's delete semantics (`deleted` flag retained, redundant
  deletes dropped). Plan 016 (media lifecycle) and any "purge deleted
  messages" feature must update them deliberately.
- Reviewer: check the blocked-delete release test asserts the **bound** native
  id, not just dispatch count — that binding (`bind_native_parent`) is the
  subtle part.
