# Plan 030: Collapse the dispatch types into one `Dispatch`

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: confirm plans 019, 026, 027, 028 and 029 are
> DONE in `plans/README.md`, then run
> `git diff --stat 5c38943..HEAD -- crates/wuphf-model/src/dispatch.rs crates/wuphf-core/src crates/wuphf-storage/src crates/wuphf-runtime/src`.
> Those plans' changes are expected. Compare the "Current state" excerpts
> (which already show their names) to the live code; any other mismatch is a
> STOP condition.

## Status

- **Priority**: P2
- **Effort**: M–L
- **Risk**: MED — the stored dispatch JSON changes shape; old rows are read through a legacy decoder
- **Depends on**: plans/019-settlement-single-source.md, 026, 027, 028, 029
- **Category**: tech-debt
- **Planned at**: commit `5c38943`, 2026-10-21

## Why this matters

One queued instruction to one connector is spread across seven types today:
`DispatchCommand` (what to do and where), `Dispatch` (that plus an id and a
state), `Action` (what to do), `TargetAddress` (where), `ProvisionThread`
(create a thread first), `BlockedDispatch` (a command plus "wait for this
message") and `RoutingOutcome` (two lists: ready commands and blocked ones).
"Is it blocked" lives in a separate list, then gets converted into
`DispatchState::Blocked` by storage. Six of the seven `Action` variants repeat
the same `message_id` and `replica_set_id` fields, so helper methods
(`message_id()`, `message_replica_set_id()`) exist only to dig them out.

After this plan there are three types: `Dispatch { id, state, to, replica_set_id, action }`,
`Target` and `Action`. The router returns finished `Dispatch` values whose
state is already `Pending` or `Blocked`, and storage just saves them. Delivery
acks and seen receipts become one `Action::Receipt { kind }`. The gRPC wire
format does not change.

## Current state

After plans 026–029, `crates/wuphf-model/src/dispatch.rs` defines (bodies
abbreviated, all derive `Clone, Debug, PartialEq, Eq, Serialize, Deserialize`
except the two marked):

```rust
pub enum Action {
    Send { message: Message, reply: Option<ReplyContext> },
    Edit { message: Message, native_id: String },
    Delete { message: Message, native_id: String },
    RenameThread { replica_set_id: ReplicaSetId, title: String },
    React { message_id: MessageId, replica_set_id: ReplicaSetId, native_id: String, reactor: String, reactor_name: Option<String>, emoji: Option<String> },
    DeliveryAck { message_id: MessageId, replica_set_id: ReplicaSetId, native_id: String },
    SeenReceipt { message_id: MessageId, replica_set_id: ReplicaSetId, native_id: String },
}   // each renamed variant/field carries a #[serde(alias = …)] from plan 027

pub struct ProvisionThread { pub container: String, pub title: String }
pub struct TargetAddress { pub address: ChatAddress, #[serde(default, alias = "provision_thread")] pub create_thread: Option<ProvisionThread> }
pub struct DispatchCommand { pub target_endpoint: EndpointId, pub target_connector: ConnectorId, pub target: TargetAddress, pub payload: Action }
impl DispatchCommand {
    pub fn message_id(&self) -> Option<MessageId> { … }      // None only for RenameThread
    pub fn records_copy(&self) -> bool { … }                // true for Send, Edit, Delete
    pub fn message_replica_set_id(&self) -> ReplicaSetId { … }
}
pub enum DispatchState { Pending, Blocked { waiting_for: MessageId }, Acknowledged { native_id: String }, Failed { reason: String } }
pub struct Dispatch { pub id: DispatchId, pub command: DispatchCommand, pub state: DispatchState }
pub struct RoutingOutcome { pub dispatches: Vec<DispatchCommand>, pub blocked: Vec<BlockedDispatch> }   // derives Clone, Debug, Default, PartialEq, Eq only
pub struct BlockedDispatch { pub command: DispatchCommand, pub waiting_for_message: MessageId }      // no serde
pub enum IngestOutcome { Routed { message_id, dispatches: Vec<Dispatch>, blocked: usize }, Deferred { message_id }, Ignored(IgnoreReason) }
```

A fact the new `Target` relies on, verified in the router: when a dispatch
must create a thread, its `container` always equals its target chat id. In
`crates/wuphf-core/src/router.rs` (`resolve_target`):

```rust
None => TargetAddress {
    address: ChatAddress::new(endpoint.chat_id.clone(), None),
    create_thread: Some(ProvisionThread {
        container: endpoint.chat_id.clone(),
        title: provision_title.to_owned(),
    }),
},
```

and `crates/wuphf-core/src/acknowledge.rs` (`persist_provisioned_thread`)
falls back to `target.address.chat_id` when there is no provision. So
`ProvisionThread` reduces to a title.

Where dispatches are built or read (all must change):

| File | What it does with dispatches |
|---|---|
| `crates/wuphf-core/src/router.rs` | `route`, `route_edit`, `route_delete`, `route_reaction`, `route_receipt`, `route_retitle` return `RoutingOutcome`; `fan_out_to_copies(message, origin, payload_for: impl Fn(&ConnectorId, String) -> Action)` (the `&ConnectorId` argument is unused by every caller); `resolve_target` returns `TargetAddress` |
| `crates/wuphf-core/src/ingest.rs` | `enqueue_outcome(&RoutingOutcome)` saves ready commands with `enqueue_dispatch(command, None)` and blocked ones with `enqueue_dispatch(blocked.command, Some(blocked.waiting_for_message))`; `route_and_record` does the same and registers provisioning when `command.target.create_thread.is_some()` |
| `crates/wuphf-core/src/acknowledge.rs` | `delivery_ack_for` builds a `DispatchCommand` with `Action::DeliveryAck`; `resume_provisioned`, `recover_provisioning`, `persist_provisioned_thread` call `message_replica_set_id()` and read `target_connector`, `target_endpoint`, `target.create_thread` |
| `crates/wuphf-core/src/send.rs` | `send_owner_message` builds the direct `DispatchCommand` with `Action::Send` |
| `crates/wuphf-storage/src/lib.rs` | `async fn enqueue_dispatch(&self, command: DispatchCommand, waiting_for: Option<MessageId>) -> StorageResult<Dispatch>` |
| `crates/wuphf-storage/src/shared.rs` | `dispatch_state(waiting_for)`; `bind_native_parent(&mut DispatchCommand, parent, native_id)`; plus the helpers plan 019 added (`acknowledged_copy`, `release_blocked`) |
| `crates/wuphf-storage/src/sqlite.rs` | `enqueue_dispatch`; `insert_dispatch` binds `command.target_endpoint` and `command.message_id()`; five places decode `Dispatch` from `dispatches.payload` (`pending_dispatches`, `dispatch` via `get_json`, two in `acknowledge_dispatch`, one in `fail_dispatch`) |
| `crates/wuphf-storage/src/memory.rs` | `enqueue_dispatch`; `acknowledge_dispatch` reads `command.target_endpoint` |
| `crates/wuphf-runtime/src/grpc.rs` | `output_to_proto` maps each `Action` variant to a `pb::server_frame::Body` variant; `provision_thread_to_proto` |
| `crates/wuphf-runtime/src/connector.rs` | `deliver_or_hold` reads `dispatch.command.target_connector` |
| Tests | `crates/wuphf-core/src/tests.rs`, `crates/wuphf-runtime/src/tests.rs`, `crates/wuphf-storage/src/tests.rs`, `crates/wuphf-model/src/legacy_json_tests.rs` (from plan 027) |

The `dispatches` table (`migrations/0007_nullable_dispatch_message_id.sql`)
stores `id, target_endpoint, message_id (nullable), waiting_for, state, payload`.
Those columns stay; only the JSON in `payload` changes shape. No migration is
needed.

## Target design

```rust
/// One instruction to one connector, tracked until it is delivered or fails.
pub struct Dispatch {
    pub id: DispatchId,
    pub state: DispatchState,
    pub to: Target,
    /// The replica set the dispatch belongs to. Keys thread provisioning.
    pub replica_set_id: ReplicaSetId,
    pub action: Action,
}

/// Where a dispatch goes.
pub struct Target {
    pub endpoint_id: EndpointId,
    pub connector_id: ConnectorId,
    pub address: ChatAddress,
    /// When set, the connector first creates a thread with this title inside
    /// `address.chat_id`, sends into it, and reports the new thread id in its
    /// acknowledgement.
    #[serde(default)]
    pub create_thread: Option<String>,
}

pub enum Action {
    Send { message: Message, reply: Option<ReplyContext> },
    Edit { message: Message, native_id: String },
    Delete { message_id: MessageId, native_id: String },
    React { message_id: MessageId, native_id: String, reactor: String, reactor_name: Option<String>, emoji: Option<String> },
    Receipt { message_id: MessageId, native_id: String, kind: ReceiptKind },
    RenameThread { title: String },
}

impl Dispatch {
    /// A new dispatch: `Pending`, or `Blocked` until `waiting_for` has been
    /// delivered to the same endpoint.
    pub fn new(to: Target, replica_set_id: ReplicaSetId, action: Action, waiting_for: Option<MessageId>) -> Self;
    /// The message the dispatch acts on; `None` for `RenameThread`.
    pub fn message_id(&self) -> Option<MessageId>;
    /// Whether acknowledging it records a delivered copy (Send, Edit, Delete).
    pub fn records_copy(&self) -> bool;
    pub fn is_pending(&self) -> bool;
}
```

Every type keeps the existing derives. `RoutingOutcome`, `BlockedDispatch`,
`DispatchCommand`, `TargetAddress` and `ProvisionThread` are deleted.
`ReceiptKind` (in `message.rs`) already exists with `Delivered` and `Read`.
Keep the doc comments from the deleted items where they still apply (for
example the `records_copy` explanation of why signals must not record a copy).

**Upgrade path for rows already in SQLite.** A database may hold dispatches
written in the old shape: acknowledged history, and pending or blocked work
that will still be delivered. Storage decodes every `dispatches.payload`
through one function that tries the new shape first and, if that fails,
decodes the old shape with private copies of the old types and converts it.
Nothing rewrites existing rows; a row is rewritten in the new shape the next
time storage updates it (acknowledge, fail, release).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Print fixtures (Step 1 only) | `cargo test -p wuphf-storage print_dispatch_json -- --ignored --nocapture` | one JSON line per sample |
| Build | `cargo build --workspace --all-targets` | exit 0 |
| Per crate | `cargo test -p wuphf-model`, `-p wuphf-storage`, `-p wuphf-core`, `-p wuphf-runtime` | exit 0 |
| All tests | `cargo test --workspace` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `crates/wuphf-model/src/dispatch.rs`, `crates/wuphf-model/src/legacy_json_tests.rs`
- `crates/wuphf-core/src/{router,ingest,acknowledge,send}.rs`, `crates/wuphf-core/src/tests.rs`
- `crates/wuphf-storage/src/{lib,shared,sqlite,memory}.rs`,
  `crates/wuphf-storage/src/legacy.rs` (create), `crates/wuphf-storage/src/tests.rs`
- `crates/wuphf-runtime/src/{grpc,connector,lib}.rs`, `crates/wuphf-runtime/src/tests.rs`
- `crates/wuphf-connector/tests/session.rs` if it reads dispatch fields

**Out of scope** (do NOT touch):
- `crates/wuphf-proto/**`, `proto/go/**`, `connectors/**`: the frames
  `MessageCreateDispatch`, `DeliveryAckDispatch`, `SeenReceiptDispatch`,
  `ProvisionThread` etc. stay exactly as they are.
- `migrations/**`: no schema change.
- `replica_set_id`: keep the name; plan 031 renames it to `conversation_id`.
- The provisioning stash and the `provisioning` cache in `Core`: keep their
  logic; only change how they read fields.
- SQLite transaction boundaries in `acknowledge_dispatch`: the same statements
  must run inside one transaction, in the same order.

## Git workflow

- Branch: `advisor/030-single-dispatch-type`.
- Commits: `test(storage): capture current dispatch JSON` (Step 1), then
  `refactor: collapse dispatch types into Dispatch, Target and Action` (Steps
  2–7; the workspace does not compile in between, so commit at the end of
  Step 7), then `test: cover legacy dispatch decoding and receipt mapping` (Step 8).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 0: Baseline

`cargo test --workspace 2>&1 | grep "^test result"`; sum the passed counts.

### Step 1: Capture today's dispatch JSON before changing anything

Add a temporary ignored test at the end of `crates/wuphf-storage/src/tests.rs`
named `print_dispatch_json`. It builds one `Dispatch` per current `Action`
variant (7 values), using fixed ids (`MessageId(uuid::Uuid::from_u128(1))`
and so on), and prints `serde_json::to_string(&dispatch)` for each, one per
line. Make the `Send` sample carry `reply: Some(ReplyContext { … })`,
`target.create_thread: Some(ProvisionThread { container: "tg-chat".into(), title: "Priya".into() })`
with `target.address.chat_id == "tg-chat"`, and state
`DispatchState::Blocked { waiting_for: … }`. Give the others `Pending`.

Run it, save the 7 printed lines to a scratch file outside the repo, then
DELETE the temporary test (it will not compile after Step 2).

**Verify**: you have 7 JSON lines, and `cargo test -p wuphf-storage` → exit 0
after deleting the test.

### Step 2: Replace the model types

In `crates/wuphf-model/src/dispatch.rs`, replace `Action`, `ProvisionThread`,
`TargetAddress`, `DispatchCommand`, `Dispatch`, `RoutingOutcome` and
`BlockedDispatch` with the "Target design" above. Remove the plan-027
`#[serde(alias …)]` attributes from `Action` (old JSON is now handled by the
storage legacy decoder, not by aliases on the new types). Implement:

```rust
impl Dispatch {
    pub fn new(to: Target, replica_set_id: ReplicaSetId, action: Action, waiting_for: Option<MessageId>) -> Self {
        Self {
            id: DispatchId::new(),
            state: match waiting_for {
                Some(waiting_for) => DispatchState::Blocked { waiting_for },
                None => DispatchState::Pending,
            },
            to,
            replica_set_id,
            action,
        }
    }
    pub fn message_id(&self) -> Option<MessageId> {
        match &self.action {
            Action::Send { message, .. } | Action::Edit { message, .. } => Some(message.id),
            Action::Delete { message_id, .. }
            | Action::React { message_id, .. }
            | Action::Receipt { message_id, .. } => Some(*message_id),
            Action::RenameThread { .. } => None,
        }
    }
    pub fn records_copy(&self) -> bool {
        matches!(self.action, Action::Send { .. } | Action::Edit { .. } | Action::Delete { .. })
    }
    pub fn is_pending(&self) -> bool {
        self.state == DispatchState::Pending
    }
}
```

In `crates/wuphf-model/src/legacy_json_tests.rs` (from plan 027), delete the
dispatch samples and their `LEGACY_…` constants, but first copy those
constants into your scratch file: they are pre-027 JSON and become fixtures in
Step 8. Keep the Message, MessageCopy and ObservedMessage tests.

**Verify**: `cargo test -p wuphf-model` → exit 0.

### Step 3: Storage

- `lib.rs`: `async fn enqueue_dispatch(&self, dispatch: &Dispatch) -> StorageResult<()>;`
  Update its doc comment: the caller builds the dispatch (with
  `Dispatch::new`) and storage persists it as given.
- `shared.rs`: delete `dispatch_state`. Rewrite `bind_native_parent` over a
  `Dispatch`:

```rust
pub(crate) fn bind_native_parent(dispatch: &mut Dispatch, parent: Message, native_id: String) {
    match &mut dispatch.action {
        // A blocked reply is released because the bridge just sent the parent
        // to this endpoint, so the quoted copy is always one the bridge authored.
        Action::Send { reply, .. } => *reply = Some(ReplyContext::quoting(parent, native_id, true)),
        Action::Edit { native_id: target, .. }
        | Action::Delete { native_id: target, .. }
        | Action::React { native_id: target, .. }
        | Action::Receipt { native_id: target, .. } => *target = native_id,
        // A rename is never blocked on a message.
        Action::RenameThread { .. } => {}
    }
}
```

  Adapt plan 019's `acknowledged_copy` and `release_blocked` helpers to take
  `&Dispatch` / `&mut Dispatch` (use `dispatch.records_copy()`,
  `dispatch.message_id()`, `dispatch.to.endpoint_id`).
- Create `legacy.rs` (`mod legacy;` in `lib.rs`) with private copies of the old
  shape and a decoder:

```rust
//! Reads dispatch rows written before the dispatch types were collapsed (plan
//! 030). New rows never use this shape; old rows are rewritten in the new shape
//! the next time storage updates them.

#[derive(Deserialize)]
struct LegacyDispatch { id: DispatchId, command: LegacyCommand, state: DispatchState }
#[derive(Deserialize)]
struct LegacyCommand { target_endpoint: EndpointId, target_connector: ConnectorId, target: LegacyTarget, payload: LegacyAction }
#[derive(Deserialize)]
struct LegacyTarget { address: ChatAddress, #[serde(default, alias = "provision_thread")] create_thread: Option<LegacyThread> }
#[derive(Deserialize)]
struct LegacyThread { title: String }   // `container` is ignored: it always equals address.chat_id
#[derive(Deserialize)]
enum LegacyAction {
    #[serde(alias = "MessageCreate")] Send { message: Message, #[serde(alias = "reply_context")] reply: Option<ReplyContext> },
    #[serde(alias = "MessageEdit")] Edit { message: Message, #[serde(alias = "target_platform_message_id")] native_id: String },
    #[serde(alias = "MessageDelete")] Delete { message: Message, #[serde(alias = "target_platform_message_id")] native_id: String },
    #[serde(alias = "RetitleThread")] RenameThread { replica_set_id: ReplicaSetId, title: String },
    #[serde(alias = "Reaction")] React { message_id: MessageId, replica_set_id: ReplicaSetId, #[serde(alias = "target_platform_message_id")] native_id: String, reactor: String, reactor_name: Option<String>, emoji: Option<String> },
    DeliveryAck { message_id: MessageId, replica_set_id: ReplicaSetId, #[serde(alias = "target_platform_message_id")] native_id: String },
    SeenReceipt { message_id: MessageId, replica_set_id: ReplicaSetId, #[serde(alias = "target_platform_message_id")] native_id: String },
}

/// Decode a `dispatches.payload` value in either the current or the legacy shape.
pub(crate) fn decode_dispatch(payload: &str) -> StorageResult<Dispatch> {
    match serde_json::from_str::<Dispatch>(payload) {
        Ok(dispatch) => Ok(dispatch),
        Err(error) => serde_json::from_str::<LegacyDispatch>(payload)
            .map(LegacyDispatch::into_dispatch)
            .map_err(|_| error.into()),
    }
}
```

  `into_dispatch` maps: `Send`/`Edit` keep `message` and take
  `replica_set_id` from `message.replica_set_id`; `Delete` becomes
  `Delete { message_id: message.id, native_id }`; `RenameThread`, `React` take
  their own `replica_set_id`; `DeliveryAck` becomes
  `Receipt { kind: ReceiptKind::Delivered, … }`; `SeenReceipt` becomes
  `Receipt { kind: ReceiptKind::Read, … }`; `to` is
  `Target { endpoint_id: target_endpoint, connector_id: target_connector, address, create_thread: create_thread.map(|t| t.title) }`;
  `id` and `state` copy across.
- `sqlite.rs`: `enqueue_dispatch` calls `insert_dispatch(&self.pool, dispatch)`;
  `insert_dispatch` binds `dispatch.to.endpoint_id` and `dispatch.message_id()`.
  Replace all five JSON decodes of a dispatch (including `dispatch()`, which
  currently uses `get_json`) with `crate::legacy::decode_dispatch(row.get("payload"))?`.
  Replace `dispatch.command.target_endpoint` with `dispatch.to.endpoint_id`.
- `memory.rs`: `enqueue_dispatch` inserts `dispatch.clone()`;
  `command.target_endpoint` → `to.endpoint_id`.

**Verify**: `cargo build -p wuphf-storage` → exit 0.

### Step 4: Router

In `crates/wuphf-core/src/router.rs`, every `route_*` function returns
`Result<Vec<Dispatch>, RouterError>`, each element built with
`Dispatch::new(…)`:

- `resolve_target(endpoint, conversation, provision_title) -> Result<Target, RouterError>`:
  same branches as today, producing `Target { endpoint_id: endpoint.id, connector_id: endpoint.connector_id.clone(), address, create_thread }`
  with `create_thread: Some(provision_title.to_owned())` in the
  "mirror without a thread" branch and `None` otherwise.
- `route`: a reply whose parent has no copy at this endpoint becomes
  `Dispatch::new(target, message.replica_set_id, Action::Send { message: message.clone(), reply: None }, Some(parent_id))`;
  everything else `Dispatch::new(…, None)`.
- `fan_out_to_copies(message, origin, action_for: impl Fn(String) -> Action)`:
  drop the unused `&ConnectorId` parameter. A target with a copy gets
  `Dispatch::new(target, message.replica_set_id, action_for(copy.native_id), None)`;
  one without gets `action_for(String::new())` and `Some(message.id)`.
- `route_delete`: `Action::Delete { message_id: message.id, native_id }`.
- `route_receipt`: `|native_id| Action::Receipt { message_id: message.id, native_id, kind }`
  (the `match kind` disappears).
- `route_retitle`: `Action::RenameThread { title: title.to_owned() }` with
  `conversation.replica_set_id`.

### Step 5: Core

- `ingest.rs`: replace `enqueue_outcome` with

```rust
/// Persist routed dispatches and return the ones ready to send. Blocked ones
/// are saved and surface later, when the message they wait for is acknowledged.
async fn save_dispatches(&self, dispatches: Vec<Dispatch>) -> Result<(Vec<Dispatch>, usize), CoreError>
```

  returning `(pending, blocked_count)`. `route_and_record` uses it and
  registers provisioning for pending dispatches with
  `dispatch.to.create_thread.is_some()` (the same rule as today: only ready
  dispatches register). `IngestOutcome::Routed { blocked, … }` takes the
  count. Callers that destructured `(routing, dispatches)` now receive
  `(dispatches, blocked)`.
- `acknowledge.rs`: `message_replica_set_id()` → `.replica_set_id`;
  `target.create_thread.is_some()` → `to.create_thread.is_some()`;
  `persist_provisioned_thread` uses `dispatch.to.connector_id`,
  `dispatch.to.endpoint_id`, and `container = dispatch.to.address.chat_id.clone()`
  (add a comment: a thread is always created inside the target chat).
  `delivery_ack_for` checks `matches!(dispatch.action, Action::Send { .. })`
  and builds
  `Dispatch::new(Target { endpoint_id: endpoint.id, connector_id: endpoint.connector_id.clone(), address: endpoint.address(), create_thread: None }, dispatch.replica_set_id, Action::Receipt { message_id, native_id: origin.native_id, kind: ReceiptKind::Delivered }, None)`,
  saves it with `enqueue_dispatch(&ack)` and returns it.
- `send.rs`: the direct dispatch becomes
  `Dispatch::new(Target { endpoint_id, connector_id: owner_message.connector_id.clone(), address, create_thread: None }, replica_set_id, Action::Send { message: message.clone(), reply }, None)`,
  saved with `enqueue_dispatch(&direct)`.

**Verify**: `cargo build -p wuphf-core` → exit 0.

### Step 6: Runtime

- `connector.rs`: `dispatch.command.target_connector` → `dispatch.to.connector_id`.
- `grpc.rs` `output_to_proto`: match on `dispatch.action`, reading the address
  from `dispatch.to`. The `Send` arm sets
  `provision_thread: to.create_thread.map(|title| pb::ProvisionThread { container: to.address.chat_id.clone(), title })`
  (clone the chat id before moving `to.address.chat_id` into
  `target_chat_id`). `Action::Receipt { kind: ReceiptKind::Delivered, .. }` →
  `pb::server_frame::Body::DeliveryAck(…)`; `kind: ReceiptKind::Read` →
  `pb::server_frame::Body::SeenReceipt(…)`; `RenameThread` → `MessageRetitle`
  with `target_thread_id: to.address.thread_id.unwrap_or_default()` as today.
  Delete `provision_thread_to_proto`.

**Verify**: `cargo build --workspace` → exit 0 (tests may not compile yet).

### Step 7: Update the tests mechanically

Fix `crates/wuphf-core/src/tests.rs`, `crates/wuphf-runtime/src/tests.rs`,
`crates/wuphf-storage/src/tests.rs` (and `crates/wuphf-connector/tests/session.rs`
if needed) with these substitutions, changing only how values are read or
built, never what is asserted:

| Old | New |
|---|---|
| `d.command.payload` | `d.action` |
| `d.command.target_endpoint` | `d.to.endpoint_id` |
| `d.command.target_connector` | `d.to.connector_id` |
| `d.command.target.address` | `d.to.address` |
| `d.command.target.create_thread == Some(ProvisionThread { container, title })` | `d.to.create_thread == Some(title)` (and `d.to.address.chat_id == container`) |
| `d.command.message_replica_set_id()` | `d.replica_set_id` |
| `Action::DeliveryAck { .. }` | `Action::Receipt { kind: ReceiptKind::Delivered, .. }` |
| `Action::SeenReceipt { .. }` | `Action::Receipt { kind: ReceiptKind::Read, .. }` |
| `Action::Delete { message, .. }` | `Action::Delete { message_id, .. }` |
| `storage.enqueue_dispatch(command, waiting)` | build with `Dispatch::new(…, waiting)` then `storage.enqueue_dispatch(&d)` |

**Verify**: `cargo test --workspace` → exit 0 with the Step 0 total.

### Step 8: New tests

- `crates/wuphf-model/src/dispatch.rs` (add a `#[cfg(test)] mod tests`):
  `Dispatch::new` gives `Pending` without `waiting_for` and
  `Blocked { waiting_for }` with it; `records_copy` is true exactly for
  `Send`, `Edit`, `Delete`; `message_id` is `None` only for `RenameThread`.
- `crates/wuphf-storage/src/legacy.rs` (`#[cfg(test)] mod tests`): for each of
  the 7 JSON lines from Step 1 and each pre-027 dispatch constant saved in
  Step 2, `decode_dispatch` succeeds and yields the expected `Dispatch`
  (check `to`, `replica_set_id`, `state`, and the `action` variant, including
  `DeliveryAck` → `Receipt { kind: Delivered }` and the `create_thread` title).
  Also: a new-shape dispatch round-trips through
  `serde_json::to_string` + `decode_dispatch` unchanged; and garbage input
  returns an error.
- `crates/wuphf-runtime/src/grpc.rs` (add `#[cfg(test)] mod tests`):
  `output_to_proto` maps `Receipt { kind: Delivered }` to a `DeliveryAck`
  frame and `kind: Read` to a `SeenReceipt` frame; a `Send` with
  `create_thread: Some("Priya")` produces
  `provision_thread: Some(ProvisionThread { container: <target chat id>, title: "Priya" })`.

**Verify**: `cargo test --workspace` → exit 0; total = Step 0 + the new tests.

### Step 9: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` → exit 0.

## Test plan

Summarized from Steps 7–8: the existing 46 core tests (blocked replies, edits
and reactions before delivery, thread provisioning, delivery acks, receipts),
the storage tests (blocked release on acknowledge, against both backends) and
the runtime tests must pass with assertions unchanged. New: `Dispatch`
constructor and helper tests, legacy decoding for every old variant in both
the pre-027 and post-027 spellings, and receipt and thread-creation wire
mapping.

## Done criteria

- [ ] `cargo test --workspace` exits 0 with the new tests
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings
- [ ] `grep -rnw "DispatchCommand\|TargetAddress\|ProvisionThread\|RoutingOutcome\|BlockedDispatch\|message_replica_set_id\|DeliveryAck\|SeenReceipt" crates --include='*.rs' | grep -v "pb::\|crates/wuphf-storage/src/legacy.rs"` → no matches
- [ ] `grep -c "decode_dispatch" crates/wuphf-storage/src/sqlite.rs` ≥ 5 and `grep -n "from_str::<Dispatch>\|get_json(&self.pool, \"dispatches\"" crates/wuphf-storage/src/sqlite.rs` → no matches
- [ ] `git diff --stat -- crates/wuphf-proto proto connectors migrations` → empty
- [ ] `plans/README.md` status row updated

## STOP conditions

- Plan 019 is not DONE. (It adds the memory-backend `records_copy` guard and
  the shared settlement helpers this plan adapts.)
- A test needs its assertion changed, not just its field paths. That means
  behavior moved; report the test.
- Some caller of `fan_out_to_copies` does use the connector argument.
- You find a provisioning target whose container differs from its target chat
  id (the reason `ProvisionThread` can become a title would be false).
- A legacy fixture fails to decode and the fix would mean changing the new
  types rather than `legacy.rs`.

## Maintenance notes

- `legacy.rs` can be deleted once no database holds an old-shape row.
  Check with: `SELECT count(*) FROM dispatches WHERE payload LIKE '%"command":%'`
  (old rows have a `command` key; new rows never do). Acknowledged and failed
  rows are history and are not rewritten, so in practice they stay until
  cleaned up; a later migration could delete terminal rows older than some age.
- Plan 031 renames `Dispatch.replica_set_id` to `conversation_id` and must
  extend `legacy.rs` with `#[serde(alias = "replica_set_id")]` where it reads
  that field.
- Reviewer focus: the SQLite transaction in `acknowledge_dispatch` is
  statement-for-statement the same; the provisioning registration still only
  counts ready dispatches; the wire frames for receipts are unchanged.
