# Plan 027: Rename model fields and enum variants, keeping old stored JSON readable

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 5c38943..HEAD -- crates/`
> This plan runs after plan 026, so you WILL see 026's renames. Confirm that
> 026 is marked DONE in `plans/README.md` and that the type names in this plan
> (`Dispatch`, `Action`, `Event`, `EventKind`, `NewMessage`, `DispatchAck`,
> `OwnerMessage`, `ChatAddress`) exist in `crates/wuphf-model/src/`. Any other
> drift in the excerpts below is a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED — stored JSON field names change; mitigated with serde aliases and a fixture test
- **Depends on**: plans/026-model-type-renames.md
- **Category**: tech-debt
- **Planned at**: commit `5c38943`, 2026-10-21

## Why this matters

Field and variant names in the model use three different words for the same
thing ("platform message id", "target platform message id", and in the docs
"native id"), and `sender` holds the message's *author* even though the doc
comment on `Message` says "`sender` is the chat". This plan renames those
fields and variants to one vocabulary: `native_id` for an app's own message id,
`author_id`/`author_name` for who wrote a message, and short action verbs
(`Send`, `Edit`, `Delete`, `React`, `RenameThread`).

Several of these types are stored as JSON in SQLite. serde writes field and
variant names into that JSON, so a plain rename would make every existing row
unreadable. Each renamed field or variant on a stored type therefore gets
`#[serde(alias = "<old name>")]`: new rows are written with the new name, old
rows still load. A fixture test captured *before* the renames proves it.

## Current state

After plan 026, the model types are named as below. Their fields are still
the old names. Excerpts are from `crates/wuphf-model/src/` at `5c38943`, with
026's type renames applied.

`message.rs` — `Message` (stored in `messages.payload`, and inside dispatch JSON):

```rust
pub struct Message {
    pub id: MessageId,
    pub replica_set_id: ReplicaSetId,
    pub sender: String,
    pub participant: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<MessageId>,
    pub timestamp_ms: i64,
    #[serde(default)]
    pub deleted: bool,
}
```

`message.rs` — `MessageCopy` (stored in `message_copies.payload`):
`{ message_id, endpoint_id, platform_message_id: String }`.

`message.rs` — `ReplyContext` (stored inside dispatch JSON):
`{ message_id, platform_message_id, sender, participant, text, attachments, #[serde(default)] authored_by_bridge }`.

`message.rs` — `NewMessage` (was `InboundMessage`; NOT stored):
`{ platform_message_id, sender, participant, sender_name, text, attachments, reply_to_platform_message_id, timestamp_ms }`.

`message.rs` — `EventKind` (was `EventPayload`; NOT stored):

```rust
pub enum EventKind {
    MessageCreate(NewMessage),
    MessageEdit { platform_message_id: String, text: Option<String> },
    MessageDelete { platform_message_id: String },
    ReactionSet { platform_message_id: String, reactor: String, reactor_name: Option<String>, emoji: Option<String>, from_me: bool },
    ReceiptSet { platform_message_id: String, observer: String, from_me: bool, kind: ReceiptKind },
}
```

`message.rs` — `DedupKey { platform, chat_id, platform_message_id }` (stored as
SQL columns, never as JSON).

`dispatch.rs` — `Action` (was `DispatchPayload`; stored inside `dispatches.payload`):

```rust
pub enum Action {
    MessageCreate { message: Message, reply_context: Option<ReplyContext> },
    MessageEdit { message: Message, target_platform_message_id: String },
    MessageDelete { message: Message, target_platform_message_id: String },
    RetitleThread { replica_set_id: ReplicaSetId, title: String },
    Reaction { message_id: MessageId, replica_set_id: ReplicaSetId, target_platform_message_id: String, reactor: String, reactor_name: Option<String>, emoji: Option<String> },
    DeliveryAck { message_id: MessageId, replica_set_id: ReplicaSetId, target_platform_message_id: String },
    SeenReceipt { message_id: MessageId, replica_set_id: ReplicaSetId, target_platform_message_id: String },
}
```

`dispatch.rs` — `TargetAddress { address: ChatAddress, #[serde(default)] provision_thread: Option<ProvisionThread> }` (stored).
`dispatch.rs` — `DispatchState::Acknowledged { platform_message_id: String }` (stored).
`dispatch.rs` — `DispatchAck { dispatch_id, platform_message_id, #[serde(default)] thread_id }` (NOT stored).
`dispatch.rs` — `OwnerMessage { connector_id, chat_id, thread_id, text, reply_to_platform_message_id, timestamp_ms }` (NOT stored).

`observation.rs` — `ObservedMessage` (stored in `observed_messages.payload`):
`{ connector_id, platform, chat_id, thread_id, platform_message_id, sender, sender_name, participant, text, attachments, reply_to_platform_message_id, direction, timestamp_ms, deleted, edited }`.

Facts verified at planning time:

- Only `crates/wuphf-storage` writes JSON (`serde_json::to_string` appears
  nowhere else outside tests). Events, `DispatchAck` and `OwnerMessage` are
  never persisted, so they need no aliases.
- No SQL statement reads inside JSON (no `json_extract`). SQL column names
  such as `platform_message_id` in `message_copies`, `observed_messages` and
  `seen_events` are separate from JSON keys and must NOT be renamed.
- No model type uses `deny_unknown_fields`; serde `alias` works on struct
  fields, on struct-variant fields, and on enum variants.
- The protobuf types keep their names. In `crates/wuphf-runtime/src/grpc.rs`
  and `observer_service.rs`, a line such as
  `platform_message_id: required_string(event.platform_message_id, "platform_message_id")?`
  becomes `native_id: required_string(event.platform_message_id, "platform_message_id")?`:
  only the model side (left of the colon) changes.

## The renames

Stored types (add `#[serde(alias = "<old>")]` on each renamed item):

| Type | Old | New |
|---|---|---|
| `Message` | `sender` | `author_id` |
| `Message` | `sender_name` | `author_name` |
| `ReplyContext` | `platform_message_id` | `native_id` |
| `ReplyContext` | `sender` | `author_id` |
| `MessageCopy` | `platform_message_id` | `native_id` |
| `DispatchState::Acknowledged` | `platform_message_id` | `native_id` |
| `TargetAddress` | `provision_thread` | `create_thread` |
| `Action` variant | `MessageCreate` | `Send` |
| `Action::Send` field | `reply_context` | `reply` |
| `Action` variant | `MessageEdit` | `Edit` |
| `Action` variant | `MessageDelete` | `Delete` |
| `Action` variant | `RetitleThread` | `RenameThread` |
| `Action` variant | `Reaction` | `React` |
| `Action::{Edit, Delete, React, DeliveryAck, SeenReceipt}` field | `target_platform_message_id` | `native_id` |
| `ObservedMessage` | `platform_message_id` | `native_id` |
| `ObservedMessage` | `sender` | `author_id` |
| `ObservedMessage` | `sender_name` | `author_name` |
| `ObservedMessage` | `reply_to_platform_message_id` | `reply_to_native_id` |

Not-stored types (plain rename, no alias):

| Type | Old | New |
|---|---|---|
| `NewMessage` | `platform_message_id`, `sender`, `sender_name`, `reply_to_platform_message_id` | `native_id`, `author_id`, `author_name`, `reply_to_native_id` |
| `EventKind` variants | `MessageCreate`, `MessageEdit`, `MessageDelete`, `ReactionSet`, `ReceiptSet` | `NewMessage`, `Edit`, `Delete`, `Reaction`, `Receipt` |
| `EventKind` variant fields | `platform_message_id` | `native_id` |
| `DispatchAck` | `platform_message_id` | `native_id` |
| `OwnerMessage` | `reply_to_platform_message_id` | `reply_to_native_id` |
| `DedupKey` | `platform_message_id` | `native_id` |

`participant` is deliberately NOT renamed: WhatsApp uses it to address quoted
replies, and folding it into `author_id` needs a separate decision.

Also fix the wrong doc comment on `Message` (`message.rs`, the line
"`sender` is the chat; `participant` is the individual author in a group
chat"). Replace it with: "`author_id` is the platform id of whoever wrote the
message; `participant` is set when a mirror should label the message with its
author."

Serde shape to produce, for reference:

```rust
pub struct MessageCopy {
    pub message_id: MessageId,
    pub endpoint_id: EndpointId,
    #[serde(alias = "platform_message_id")]
    pub native_id: String,
}

pub enum Action {
    #[serde(alias = "MessageCreate")]
    Send {
        message: Message,
        #[serde(alias = "reply_context")]
        reply: Option<ReplyContext>,
    },
    #[serde(alias = "MessageEdit")]
    Edit {
        message: Message,
        #[serde(alias = "target_platform_message_id")]
        native_id: String,
    },
    // … same pattern for the other variants
}
```

Keep any existing `#[serde(default)]` attributes on the renamed fields.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Model tests | `cargo test -p wuphf-model` | exit 0 |
| Print fixtures (Step 1 only) | `cargo test -p wuphf-model print_legacy_json -- --ignored --nocapture` | prints one JSON line per sample |
| Build | `cargo build --workspace --all-targets` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| All tests | `cargo test --workspace` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

`protoc` must be on PATH (`nix develop` provides it).

## Scope

**In scope**:
- `crates/wuphf-model/src/message.rs`, `dispatch.rs`, `observation.rs`
- `crates/wuphf-model/src/legacy_json_tests.rs` (create) and the
  `#[cfg(test)] mod legacy_json_tests;` line in `crates/wuphf-model/src/lib.rs`
- Every other `*.rs` file under `crates/` that fails to compile because of a
  rename (expected: `wuphf-storage` memory.rs, sqlite.rs, shared.rs, tests.rs;
  `wuphf-router` routing.rs; `wuphf-core` ingest.rs, acknowledge.rs, send.rs,
  lib.rs, tests.rs; `wuphf-runtime` grpc.rs, observation.rs,
  observer_service.rs, owner_send.rs, lib.rs, tests.rs;
  `crates/wuphf-connector/tests/session.rs`)

**Out of scope** (do NOT touch):
- SQL text in `crates/wuphf-storage/src/sqlite.rs` (column names stay) and
  `migrations/**`.
- `crates/wuphf-proto/**`, `proto/go/**`, `connectors/**`, `crates/wuphf-tui/**`.
- Function parameter and local variable names (for example the storage trait's
  `platform_message_id: &str` parameters). They may keep the old wording in
  this plan; see Maintenance notes.
- `participant`, `Message.replica_set_id`, and every `replica_set_id` field
  (renamed by plan 031).
- Global search-and-replace of `platform_message_id` or `sender`: both words
  also appear on protobuf types and in SQL. Change the model definition, then
  fix each compiler error at its site.

## Git workflow

- Branch: `advisor/027-model-field-renames` off `v2` (after 026 is merged).
- Commits: `test(model): pin legacy JSON for stored types` (Step 1), then
  `refactor(model): rename stored fields with serde aliases` (Steps 2–3),
  then `refactor(model): rename event and ack fields` (Step 4).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Capture legacy JSON fixtures BEFORE renaming anything

Create `crates/wuphf-model/src/legacy_json_tests.rs` and add
`#[cfg(test)] mod legacy_json_tests;` to `crates/wuphf-model/src/lib.rs`.

In it, write one sample-builder function per stored shape, using fixed ids
(`MessageId(uuid::Uuid::from_u128(1))`, `EndpointId(uuid::Uuid::from_u128(2))`,
and so on) so the output is deterministic:

- `sample_message() -> Message` with `sender`, `sender_name`, `participant`
  and `text` all set.
- `sample_copy() -> MessageCopy`.
- `sample_dispatches() -> Vec<Dispatch>`: one `Dispatch` per `Action` variant
  (7 total). The `MessageCreate` one has `reply_context: Some(ReplyContext{…})`
  and `target.provision_thread: Some(ProvisionThread { container, title })`.
  Give the `MessageCreate` dispatch state `DispatchState::Acknowledged {
  platform_message_id: "tg-1".into() }` and the rest `DispatchState::Pending`.
- `sample_observed() -> ObservedMessage` with every optional field set.

Add an ignored printer test:

```rust
#[test]
#[ignore]
fn print_legacy_json() {
    println!("MESSAGE {}", serde_json::to_string(&sample_message()).unwrap());
    // … one println per sample, each dispatch on its own line
}
```

Run `cargo test -p wuphf-model print_legacy_json -- --ignored --nocapture`,
copy each printed JSON string into a `const LEGACY_…: &str = r#"…"#;` in the
same file, then add one test per constant that asserts
`serde_json::from_str::<T>(LEGACY_X).unwrap() == sample_x()`. Keep the
printer test (ignored) for future use.

`uuid` is already a dependency of `wuphf-model` and `serde_json` is a
dev-dependency, so no `Cargo.toml` change is needed.

**Verify**: `cargo test -p wuphf-model` → exit 0, including the new legacy
tests (they pass trivially at this point; that is expected).

### Step 2: Rename the stored fields and variants, with aliases

Apply every row of the "Stored types" table to `message.rs`, `dispatch.rs`
and `observation.rs`, adding `#[serde(alias = "<old name>")]` to each renamed
field and variant. Update the `Message` doc comment as specified above, and
update other doc comments that name a renamed field.

Then update the sample builders in `legacy_json_tests.rs` to the new field
names (the compiler will point at each). Do NOT edit the `LEGACY_…` constants:
they must keep the old names, which is what proves the aliases work.

Add two assertions per stored type to `legacy_json_tests.rs`: serializing the
sample writes the new name (for example
`assert!(json.contains("\"native_id\""))`) and does not write the old one
(`assert!(!json.contains("\"platform_message_id\""))`).

**Verify**: `cargo test -p wuphf-model` → exit 0, all legacy tests pass.

### Step 3: Fix every caller of the stored fields

Run `cargo build --workspace --all-targets` and fix each error at its site.
Patterns you will meet:

- Model access: `message.sender` → `message.author_id`,
  `copy.platform_message_id` → `copy.native_id`,
  `DispatchState::Acknowledged { platform_message_id }` →
  `DispatchState::Acknowledged { native_id }`,
  `Action::MessageCreate { message, reply_context }` →
  `Action::Send { message, reply }`,
  `target.provision_thread` → `target.create_thread`.
- Wire mapping in `crates/wuphf-runtime/src/grpc.rs` (`output_to_proto`):
  the `pb::` field names stay, the model side changes, e.g.
  `sender: message.sender,` → `sender: message.author_id,` and
  `sender_name: message.sender_name,` → `sender_name: message.author_name,`.
- `crates/wuphf-runtime/src/observer_service.rs` (`observed_message_to_proto`):
  `platform_message_id: observed.platform_message_id` →
  `platform_message_id: observed.native_id`, and likewise for `sender`,
  `sender_name`, `reply_to_platform_message_id`.
- `crates/wuphf-storage/src/sqlite.rs`: Rust expressions such as
  `.bind(&copy.platform_message_id)` become `.bind(&copy.native_id)`; the SQL
  strings stay exactly as they are.

**Verify**: `cargo build --workspace --all-targets` → exit 0.

### Step 4: Rename the not-stored fields and variants

Apply the "Not-stored types" table (no aliases). Fix callers the same way.
Call sites to expect: `crates/wuphf-core/src/lib.rs` (`ingest` matches every
`EventKind` variant), `crates/wuphf-core/src/ingest.rs`,
`crates/wuphf-runtime/src/lib.rs` (`ingest_event` kind labels:
`EventKind::NewMessage(_) => "create"` and so on, keeping the label strings),
`crates/wuphf-runtime/src/observation.rs`, `crates/wuphf-runtime/src/grpc.rs`
(`input_from_proto`), `crates/wuphf-core/src/send.rs`, and tests.

**Verify**: `cargo build --workspace --all-targets` → exit 0.

### Step 5: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0; passed count = the count before this plan plus the new legacy tests.

## Test plan

- New: `crates/wuphf-model/src/legacy_json_tests.rs`. For every stored shape
  (Message, MessageCopy, 7 Dispatch variants, ObservedMessage), the old JSON
  captured in Step 1 deserializes into the expected value, and serializing
  writes only the new names.
- Existing: the storage tests (`crates/wuphf-storage/src/tests.rs`) run every
  scenario against SQLite and memory, so a missed rename on a stored type shows
  up there; the core and runtime suites cover the event and ack renames.

## Done criteria

- [ ] `cargo test --workspace` exits 0; the legacy JSON tests exist and pass
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings
- [ ] `cargo fmt --check` exits 0
- [ ] `grep -n "pub sender\b\|pub sender_name\|pub platform_message_id\|target_platform_message_id\|reply_to_platform_message_id\|provision_thread\|reply_context" crates/wuphf-model/src/*.rs` → matches only inside `#[serde(alias = …)]` attributes and the `LEGACY_` constants
- [ ] `git diff v2 -- crates/wuphf-storage/src/sqlite.rs | grep -E '^[-+].*(SELECT|INSERT|UPDATE|DELETE|WHERE|VALUES|CONFLICT)'` → no output (no SQL text changed, only Rust expressions)
- [ ] `git diff --stat -- crates/wuphf-proto proto connectors migrations crates/wuphf-tui` → empty
- [ ] `plans/README.md` status row updated

## STOP conditions

- Plan 026 is not DONE, or its new type names are not present.
- A storage test fails after Step 3. That means a stored row no longer
  round-trips; report the test name rather than editing the test's
  expectations.
- You find a place that serializes an event, `DispatchAck` or `OwnerMessage`
  to JSON (the "not stored" assumption is false). Report it; those types would
  then need aliases too.
- Fixing a compile error appears to require changing SQL text or a protobuf
  field name.

## Maintenance notes

- New rows are written with the new names; old rows keep the old names until
  rewritten. The aliases must stay until every old row has been rewritten or
  deleted. Removing them is a separate decision, best done with a migration
  that rewrites `dispatches`, `messages`, `message_copies` and
  `observed_messages` payloads.
- Follow-up deliberately deferred: rename the storage trait's
  `platform_message_id` parameters and local variables to `native_id` for
  consistency. Purely cosmetic; no data impact.
- Plans 030 and 031 rely on the names introduced here (`Action::Send`,
  `native_id`, `create_thread`).
- Reviewer focus: every renamed item on a stored type carries an alias; SQL
  and protobuf names are untouched.
