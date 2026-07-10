# Plan 023: Consolidate the model's similar types behind shared constructors

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f3bff96..HEAD -- crates/wuphf-model crates/wuphf-core/src/send.rs crates/wuphf-core/src/ingest.rs crates/wuphf-router/src/lib.rs crates/wuphf-router/src/routing.rs crates/wuphf-runtime/src/observation.rs crates/wuphf-tui/src/app.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S-M
- **Risk**: LOW
- **Depends on**: none (see coordination note on plan 020)
- **Category**: tech-debt
- **Planned at**: commit `f3bff96`, 2026-07-10

## Why this matters

The model deliberately keeps one type per pipeline stage —
`InboundMessage` (connector report) → `Message` (canonical) →
`ObservedMessage` (observation log) → `ReplyContext` (quoted parent) — and
they share a large field cluster (`sender`, `participant`, `sender_name`,
`text`, `attachments`, timestamps). The types themselves are right: each
stage has different addressing, and every one of them is persisted as serde
JSON in sqlite, so reshaping them would corrupt existing databases. What is
*wrong* is that the stage-to-stage **field mappings live at the call sites**,
scattered across three crates. Adding one field to the cluster (as
`sender_name` and `animated` were added recently) means hunting down every
literal construction: today that is `ingest.rs`, `send.rs`,
`observation.rs`, and `lib.rs` in the router, and a missed site compiles
fine while silently dropping the field (every one of these mappings is
`struct literal from another struct's fields` — the compiler only catches
*missing* fields, not fields filled with the wrong/default source).

This plan moves each mapping into a constructor on the target type in
`wuphf-model`, next to the fields it copies, so the next added field is a
one-place change per stage transition. It also folds the duplicated
string-id newtype boilerplate into the existing macro pattern and names the
TUI's repeated `(connector_id, chat_id)` chat-identity checks.

**Boundary with plan 020**: 020 deduplicates the *fan-out loops* in
`wuphf-router/src/routing.rs` and the *enqueue blocks* in
`wuphf-core/src/ingest.rs` (behavioral skeleton, no type changes). This plan
touches only type construction sites. The two plans overlap in `ingest.rs`
but in different functions (020: `ingest_edit`/`ingest_delete`/
`ingest_reaction`/`ingest_receipt`/`maybe_retitle`; 023: `ingest_create`
only) — either order works, rebase friction is minimal.

## Types to refactor and their targets

| # | Type(s) today | Duplication | Target |
|---|---------------|-------------|--------|
| 1 | `RuleId`, `ConnectorId` (`wuphf-model/src/ids.rs:59-78`) | Two hand-written string newtypes, each with an identical `Display` impl, next to the `uuid_id!` macro that already exists for the six UUID ids | `string_id!` macro in `ids.rs`, mirroring `uuid_id!` |
| 2 | `Message` ← `InboundMessage` (`wuphf-core/src/ingest.rs:65-76`) | Field-by-field literal in `ingest_create` | `Message::from_inbound(inbound, replica_set_id, reply_to)` in `wuphf-model/src/message.rs` |
| 3 | `ReplyContext` ← parent `Message` (`wuphf-router/src/lib.rs:115-124` free fn `reply_context`; `wuphf-core/src/send.rs:186-193` literal in `resolve_reply`) | The same 6-field mapping written twice in two crates | `ReplyContext::quoting(parent, platform_message_id)` in `wuphf-model/src/message.rs`; delete the router's free fn |
| 4 | `ObservedMessage` ← `SourceAddress` + `InboundMessage` (`wuphf-runtime/src/observation.rs:84-100`) | 16-field literal in `Observer::observe` | `ObservedMessage::incoming(&source, platform, &inbound)` in `wuphf-model/src/observation.rs` |
| 5 | TUI chat identity `(connector_id, chat_id)` (`wuphf-tui/src/app.rs:101-113,157-159,177-183,203-209`) | The pairwise equality check and the "re-pin selection after list mutation" dance each appear 3-4 times over the proto types | Private helpers on `App`: `chat_position`, `selected_key`, `is_open_chat` |

## Considered and rejected (do NOT do these)

Recorded so nobody re-audits them; they all look like "similar types" but
are load-bearing as-is:

- **Merging the field cluster into a shared `MessageContent` struct**
  (embedded or `#[serde(flatten)]`ed into `Message`/`InboundMessage`/
  `ObservedMessage`/`ReplyContext`): every one of these types is persisted
  as serde JSON in sqlite `payload` columns (`wuphf-storage/src/sqlite.rs`)
  and `Message` additionally travels inside `DispatchPayload`. Flatten would
  keep the JSON shape but makes serde behavior subtle (flatten +
  `#[serde(default)]` interactions), and `.content.sender` access noise
  spreads to every consumer. Constructors give the same one-place-per-field
  guarantee without touching the serialized shape.
- **Collapsing `DispatchPayload::Reaction`/`DeliveryAck`/`SeenReceipt`'s
  shared trio** (`message_id`, `replica_set_id`,
  `target_platform_message_id`) into a common struct: same persisted-JSON
  concern, and the matching is already centralized in the
  `DispatchCommand::message_id`/`records_copy`/`message_replica_set_id`
  accessors (`wuphf-model/src/dispatch.rs:113-149`). The variants also map
  to *distinct* proto dispatch messages, so the wire boundary would still
  match them separately.
- **A shared key struct for `ObservedChat`/`ObservedMessage`/`Alias`/
  `SourceAddress` (`connector_id` + address)**: reshapes persisted JSON;
  each type's extra fields dominate; nesting buys nothing at the SQL layer
  which addresses these columns directly.
- **`InboundMessage` reuse inside `EventPayload::MessageEdit`/`Delete`**:
  edits/deletes deliberately carry only what the platform reports; padding
  them to the full shape would invent data.
- **TUI `message_preview` / `attachment_label` duplicating
  `ObservedMessage::preview`**: the TUI operates on prost-generated proto
  types, not model types, and the duplication is documented as a deliberate
  mirror of the server rule (`wuphf-tui/src/app.rs:381`). Sharing would
  drag `wuphf-model` into the TUI for two tiny functions.
- **grpc.rs / observer_service.rs proto-mapping verbosity**: already
  rejected in the 2026-07-07 audit (`plans/README.md`) — wire-boundary code;
  macro-izing it hides the contract.

## Current state

- Baseline (2026-07-10, `f3bff96`): `cargo test --workspace` = 66 passing
  (13 + 17 + 29 + 7 across suites), 0 failing.
- `wuphf-model/src/ids.rs:8-32` — `uuid_id!` macro; lines 59-78 — the two
  string ids written out by hand.
- `wuphf-core/src/ingest.rs:65-76` — `Message` literal; note
  `inbound.platform_message_id` is consumed *after* the literal (line 83,
  `put_received_copy`), so the constructor call must clone the id out first.
- `wuphf-router/src/lib.rs:115-124` — free `fn reply_context`, used once at
  `routing.rs:58`.
- `wuphf-core/src/send.rs:186-193` — the second `ReplyContext` literal.
- `wuphf-runtime/src/observation.rs:84-100` — the `ObservedMessage` literal
  (`direction: Incoming`, `deleted/edited: false`). The *outgoing* template
  in `wuphf-runtime/src/raw_send.rs:83-105` maps from `RawOutbound`, which
  lives in `wuphf-core` — the model cannot see it, so that mapping stays
  where it is (it is the only one of its kind).
- `wuphf-tui/src/app.rs` — `set_chats` (101-113), `on_live` (157-159),
  `touch_chat` (177-183, 203-209).

Conventions: model types carry doc comments explaining semantics; private
helpers live below the public methods in the same file.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full suite | `cargo test --workspace` | exit 0; 66 tests pass |
| Model only | `cargo test -p wuphf-model` | exit 0 |
| Lints | `cargo clippy --workspace --all-targets` | exit 0, no new warnings |

Note: the build needs `protoc` on PATH (nix dev shell provides it).

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-model/src/ids.rs`
- `crates/wuphf-model/Cargo.toml` (add `serde_json` as a dev-dependency for
  the serde-shape guard test)
- `crates/wuphf-model/src/message.rs`
- `crates/wuphf-model/src/observation.rs`
- `crates/wuphf-core/src/ingest.rs` (only `ingest_create`)
- `crates/wuphf-core/src/send.rs` (only `resolve_reply`)
- `crates/wuphf-router/src/lib.rs`, `crates/wuphf-router/src/routing.rs`
  (only the `reply_context` fn and its call site)
- `crates/wuphf-tui/src/app.rs`

**Out of scope** (do NOT touch, even though they look related):
- Any struct's **fields, derives, or serde attributes** — every model type's
  JSON shape is persisted (sqlite) or asserted (tests); this plan adds
  constructors only.
- `crates/wuphf-runtime/src/raw_send.rs` — the `RawOutbound → ObservedMessage`
  template stays; see "Current state".
- The fan-out/enqueue duplication — that is plan 020.
- `crates/wuphf-storage/**`, `crates/wuphf-runtime/src/grpc.rs`,
  `crates/wuphf-runtime/src/observer_service.rs`, proto files, Go connectors.

## Git workflow

- Branch: `advisor/023-type-dedup` off `v2`.
- One commit: `refactor: move stage-to-stage type mappings into model constructors`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: `string_id!` macro

In `ids.rs`, add a `string_id!` macro beside `uuid_id!` (same shape: doc
attr, derives `Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize`,
`Display` delegating to the inner `String`; **no** `new()`/`Default` — these
ids come from config, never minted). Replace the hand-written `RuleId` and
`ConnectorId` with macro invocations, keeping their doc comments verbatim.

**Verify**: `cargo test -p wuphf-model` → exit 0 (newtype serde shape is
unchanged, so nothing else moves).

### Step 2: `Message::from_inbound` and `ReplyContext::quoting`

In `message.rs`:

```rust
impl Message {
    /// Promote a resolved inbound message to the canonical logical form,
    /// minting its internal id. `reply_to` is the already-resolved parent.
    pub fn from_inbound(
        inbound: InboundMessage,
        replica_set_id: ReplicaSetId,
        reply_to: Option<MessageId>,
    ) -> Self { /* field moves; id: MessageId::new(), deleted: false */ }
}

impl ReplyContext {
    /// Quote a stored parent message under the native id its copy carries
    /// at the target endpoint.
    pub fn quoting(parent: Message, platform_message_id: String) -> Self
    { /* message_id: parent.id + field moves */ }
}
```

Rewire the three call sites:
- `ingest_create`: clone `inbound.platform_message_id` into a local before
  the call (it is still needed for `put_received_copy`), then
  `Message::from_inbound(inbound, replica_set_id, reply_to)`.
- `send.rs` `resolve_reply`: `ReplyContext::quoting(parent, platform_message_id.to_owned())`.
- Router: delete `reply_context` from `lib.rs`, call
  `ReplyContext::quoting(parent, copy.platform_message_id)` in `routing.rs`,
  fix imports.

**Verify**: `cargo test --workspace` → 66 pass;
`grep -rn "fn reply_context" crates/wuphf-router` → no matches.

### Step 3: `ObservedMessage::incoming`

In `observation.rs`:

```rust
impl ObservedMessage {
    /// Capture an inbound create as an observation-log row (incoming, not
    /// yet edited or deleted). Borrows because the ingest path still owns
    /// the event afterwards.
    pub fn incoming(source: &SourceAddress, platform: Platform, inbound: &InboundMessage) -> Self
}
```

Rewire `Observer::observe` in `wuphf-runtime/src/observation.rs` to call it.

**Verify**: `cargo test -p wuphf-runtime` → exit 0.

### Step 4: TUI chat-identity helpers

In `app.rs`, add private methods on `App`:
- `fn chat_position(&self, connector_id: &str, chat_id: &str) -> Option<usize>`
- `fn selected_key(&self) -> Option<(String, String)>` (identity of the
  selected chat, for re-pinning across list mutations)
- `fn is_open_chat(&self, message: &pb::ObservedMessage) -> bool`

Use them in `set_chats`, `on_live`, and `touch_chat`. Behavior must be
byte-identical (same `.unwrap_or(0).min(len-1)` clamping in `set_chats`).

**Verify**: `cargo test -p wuphf-tui` → exit 0 (13 tests).

## Test plan

No new behavior, so the existing 66 tests are the net; they cover every
rewired path (`ingest_create` via the core suite's create/route tests,
`resolve_reply` and the router's reply context via the reply tests,
`Observer::observe` via the runtime suite, the TUI transitions via the app
tests). Add exactly one test: a round-trip guard in `wuphf-model` asserting
`RuleId`/`ConnectorId` serde as bare JSON strings (pins the macro to the
persisted shape). Run `cargo clippy --workspace --all-targets` at the end.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `cargo test --workspace` exits 0 with ≥ 67 tests passing (66 baseline + 1 serde-shape guard)
- [ ] `grep -rn "fn reply_context" crates/wuphf-router` → no matches
- [ ] `grep -c "string_id!" crates/wuphf-model/src/ids.rs` → 2 (the two invocations; the `macro_rules!` definition line has no `!` after the name)
- [ ] `ingest_create` and `Observer::observe` contain no multi-line struct literal for `Message`/`ObservedMessage`
- [ ] `git diff --stat` touches no file outside the in-scope list
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Any constructor would need to *change* a field's serde attribute, derive,
  or type to compile — that means the mapping was not as mechanical as this
  plan claims.
- A call site turns out to construct one of these types with values that do
  NOT come from the source type this plan names (e.g. an `ObservedMessage`
  built with a non-`Incoming` direction on the ingest path) — the difference
  may be load-bearing.
- Test count drops below 66 or any test fails after a step and the fix is
  not an obvious mechanical slip.

## Maintenance notes

- Adding a field to the shared cluster now means: add it to each type that
  carries it plus the constructors in `message.rs`/`observation.rs` — the
  compiler's missing-field errors on the constructors enumerate every
  mapping to update. The `RawOutbound → ObservedMessage` template in
  `raw_send.rs` is the one remaining literal; the compiler will flag it too.
- Reviewer should scrutinize: `from_inbound` moves (not clones) the
  attachment vector; `quoting` keeps `message_id: parent.id`; the TUI
  helpers preserve the `set_chats` clamp order.
- Deferred: nothing. The rejected list above is final unless the storage
  layer stops persisting these types as JSON.
