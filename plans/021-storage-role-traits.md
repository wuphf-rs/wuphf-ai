# Plan 021: Split the `Storage` god-trait into role traits and narrow consumer bounds

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 52c5084..HEAD -- crates/wuphf-storage crates/wuphf-runtime crates/wuphf-router crates/wuphf-core`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. In particular, if plan 019
> (`plans/019-settlement-single-source.md`) has landed, the backends will call
> `shared.rs` helpers inside `acknowledge_dispatch` — that is expected and
> does not block this plan.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW
- **Depends on**: plans/019-settlement-single-source.md (soft — both edit the
  backends; do 019 first to avoid conflicts, or coordinate)
- **Category**: tech-debt
- **Planned at**: commit `52c5084`, 2026-07-07

## Why this matters

`Storage` is a single trait with 33 methods spanning five unrelated concerns:
routing topology, logical messages/copies, media blobs, the dispatch queue,
and the observation log. Every consumer is generic over all of it: the
read-only `Observer` uses five observation methods but its type parameter
grants it `fail_dispatch`; the gRPC observer service — deliberately a
read-only surface guarded by a bearer token — could call any write in the
system. Each new concern (the observation log added five methods recently)
grows one monolith that every backend and every mock must implement wholesale.
Splitting the trait into role traits keeps `Storage` as the sum (so `Core`
and the backends change mechanically), gives read-only consumers read-shaped
capabilities, and makes the next concern an additive trait instead of another
god-trait growth ring. Two methods (`put_contact`, `put_chat_settings`) turn
out to have no caller outside the storage crate and are removed.

## Current state

- `crates/wuphf-storage/src/lib.rs` — `pub trait Storage: Send + Sync + 'static`
  with all 33 methods (lines 46–178). Exports `InMemoryStorage`,
  `SqliteStorage`, `StorageError`, `StorageResult`.
- `crates/wuphf-storage/src/memory.rs` / `sqlite.rs` — each has one
  `#[async_trait] impl Storage for …` block implementing everything.
- Consumers and the methods they actually use:
  - `crates/wuphf-router/src/lib.rs:94-106` — `Router<S: Storage>`; uses
    `replica_set`, `endpoint`, `conversation_for_replica_set`,
    `thread_for_conversation`, `copy_at_endpoint`, `message` (topology +
    message reads).
  - `crates/wuphf-core/src/lib.rs:17-55` — `Core<S: Storage>`; uses most
    concerns; stays bound to the full `Storage`.
  - `crates/wuphf-runtime/src/observation.rs:21-33` — `Observer<S: Storage>`;
    uses only `log_observed`, `mark_observed_edited`, `mark_observed_deleted`
    (and hands its storage to the service below).
  - `crates/wuphf-runtime/src/observer_service.rs:26-40` —
    `GrpcObserverService<S: Storage>`; uses only `list_observed_chats`,
    `observed_messages`.
  - `crates/wuphf-runtime/src/media.rs:37-38` — `GrpcMediaService<S: Storage>`;
    uses only `put_media`, `media`, `prune_unreferenced_media_before`.
  - `crates/wuphf-runtime/src/lib.rs`, `wuphf-cli/src/main.rs` — plumb
    `Arc<S: Storage>` through; stay on full `Storage`.
- `put_contact` (lib.rs:120) and `put_chat_settings` (lib.rs:121):
  `grep -rn "put_contact\|put_chat_settings" crates/` matches only
  `wuphf-storage/src/{lib,memory,sqlite}.rs` — no external caller.
- There is no `dyn Storage` anywhere (`grep -rn "dyn Storage" crates/` is
  empty), so this is purely a generics change; no object-safety concerns.
- Repo convention (AGENTS.md): "Keep persistence behind the `Storage` trait."
  The sum trait keeps that contract — role traits are its parts, not a bypass.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full suite | `cargo test --workspace` | exit 0; 52+ tests pass |
| Storage only | `cargo test -p wuphf-storage` | exit 0 |
| Type-check everything | `cargo build --workspace` | exit 0 |

Note: the build needs `protoc` on PATH (`brew install protobuf` on macOS).

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-storage/src/lib.rs`
- `crates/wuphf-storage/src/memory.rs`
- `crates/wuphf-storage/src/sqlite.rs`
- `crates/wuphf-runtime/src/observation.rs`
- `crates/wuphf-runtime/src/observer_service.rs`
- `crates/wuphf-runtime/src/media.rs`
- `crates/wuphf-router/src/lib.rs` + `crates/wuphf-router/src/routing.rs`
  (bound narrowing only)

**Out of scope** (do NOT touch, even though they look related):
- Method bodies in the backends — this plan moves methods between `impl`
  blocks verbatim; zero behavior change.
- `crates/wuphf-core/**`, `crates/wuphf-cli/**`, `crates/wuphf-runtime/src/lib.rs`
  — they legitimately need full `Storage`; leave their bounds alone.
- `migrations/` — the unused `contacts`/`chat_settings` tables (if present)
  stay; dropping schema is not worth a migration here.
- `crates/wuphf-model/**` — keep the `Contact`/`ChatSettings` types even after
  their trait methods go (they are model documentation of platform data;
  removing them is a separate decision).

## Git workflow

- Branch: `advisor/021-storage-role-traits` off `v2`.
- Commits: `refactor: split Storage into role traits`, then
  `refactor: narrow observer/media consumers to their storage roles`, then
  `chore: drop unused contact and chat-settings storage methods`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Define the role traits and the `Storage` sum

In `crates/wuphf-storage/src/lib.rs`, split the existing method list into five
`#[async_trait]` traits, each `Send + Sync + 'static`, keeping every method
signature and doc comment character-for-character:

- `TopologyStore` — `put_endpoint`, `endpoint`, `put_replica_set`,
  `replica_set`, `replica_set_for_endpoint`, `put_alias`, `resolve_endpoint`,
  `put_conversation`, `conversation`, `conversation_for_source`,
  `conversation_for_replica_set`, `put_thread_mapping`,
  `thread_for_conversation`, `conversation_for_thread`
- `MessageStore` — `claim_event`, `put_message`, `message`,
  `put_received_copy`, `record_sent_copy`, `is_sent_copy`,
  `message_id_for_copy`, `copy_at_endpoint`, `received_copy`
  (also `put_contact`, `put_chat_settings` until Step 4 removes them)
- `MediaStore` — `put_media`, `media`, `prune_unreferenced_media_before`
- `DispatchStore` — `enqueue_dispatch`, `pending_dispatches`, `dispatch`,
  `acknowledge_dispatch`, `fail_dispatch`
- `ObservationStore` — `log_observed`, `mark_observed_deleted`,
  `mark_observed_edited`, `list_observed_chats`, `observed_messages`

Then replace the monolith with a sum plus blanket impl, so every existing
`S: Storage` bound keeps compiling unchanged:

```rust
/// The full persistence surface the core needs — the sum of the role traits.
/// Blanket-implemented, so a backend only implements the five roles.
pub trait Storage:
    TopologyStore + MessageStore + MediaStore + DispatchStore + ObservationStore
{
}
impl<T> Storage for T where
    T: TopologyStore + MessageStore + MediaStore + DispatchStore + ObservationStore
{
}
```

Export the five new traits from the crate root.

**Verify**: `cargo build -p wuphf-storage` → fails only in `memory.rs`/`sqlite.rs`
(their single `impl Storage` block is now invalid) — expected; proceed.

### Step 2: Split the backend impl blocks

In `memory.rs` and `sqlite.rs`, replace `impl Storage for X` with five
`#[async_trait] impl <Role> for X` blocks, moving each method body verbatim.
Do not reorder logic or edit bodies.

**Verify**: `cargo test --workspace` → exit 0, baseline count. (The blanket
impl makes both backends `Storage` automatically.)

### Step 3: Narrow the read-shaped consumers

- `observation.rs`: `Observer<S>` bound `S: Storage` → `S: ObservationStore`.
- `observer_service.rs`: `GrpcObserverService<S>` bound → `S: ObservationStore`.
- `media.rs`: `GrpcMediaService<S>` bound → `S: MediaStore`.
- `router`: `Router<S>` bounds in `lib.rs:101` and `routing.rs:15` →
  `S: TopologyStore + MessageStore`.

Adjust `use wuphf_storage::…` imports accordingly. Note: `Observer` is
constructed from `core.storage()` in `wuphf-runtime/src/lib.rs:83`, where
`S: Storage` — that still satisfies `S: ObservationStore` via supertrait, so
no change is needed there.

**Verify**: `cargo test --workspace` → exit 0, baseline count.

### Step 4: Remove the dead contact/settings methods

Delete `put_contact` and `put_chat_settings` from `MessageStore` and from both
backend impls (including any backend-private state they wrote, e.g. the memory
maps and sqlite INSERTs — but only the code, not migrations). First re-verify
they are dead: `grep -rn "put_contact\|put_chat_settings" crates/` must match
only the definitions you are deleting. If anything else matches, STOP.

**Verify**: `cargo test --workspace` → exit 0;
`grep -rn "put_contact" crates/` → no matches.

## Test plan

No new tests: a type-level refactor with verbatim method moves. The full
workspace suite (52+ tests, including the storage tests in
`crates/wuphf-storage/src/tests.rs` that exercise both backends) is the gate.
If Step 4 deletes memory-backend state fields, `cargo build` catching unused
fields is part of the verification.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `cargo test --workspace` exits 0 with at least the baseline test count
- [ ] `crates/wuphf-storage/src/lib.rs` defines the five role traits and `Storage` as their blanket-implemented sum
- [ ] `grep -n "S: ObservationStore" crates/wuphf-runtime/src/observation.rs crates/wuphf-runtime/src/observer_service.rs` → one match in each
- [ ] `grep -rn "put_contact\|put_chat_settings" crates/` → no matches
- [ ] `git status` shows no modified files outside the in-scope list
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Any consumer turns out to need a method from a role you didn't give it
  (compile error in a file outside the in-scope list) — the role partition is
  then wrong; report the method and caller instead of widening bounds ad hoc.
- `put_contact`/`put_chat_settings` have a caller outside
  `crates/wuphf-storage/src/{lib,memory,sqlite}.rs`.
- The blanket impl conflicts with some existing impl (coherence error).
- Plan 019 is mid-flight on the same backend files (check
  `plans/README.md` status first).

## Maintenance notes

- New persistence concerns should get their own role trait added to the
  `Storage` sum, not methods appended to an existing role.
- Reviewer should scrutinize: method bodies byte-identical across the move
  (`git diff --color-moved=dimmed-zebra` makes this obvious); bounds narrowed
  only on the four consumers named in Step 3.
- Deferred: removing the `Contact`/`ChatSettings` model types and their tables
  (kept deliberately — see Scope); read/write splitting *within* a role (e.g.
  a read-only `ObservationRead`) — do it only if a real consumer needs it.
