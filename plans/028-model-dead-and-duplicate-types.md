# Plan 028: Delete dead and duplicate model types

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 5c38943..HEAD -- crates/ migrations/`
> This plan runs after plans 026 and 027, so their renames are expected.
> Confirm both are DONE in `plans/README.md`. Then confirm each excerpt below
> still matches (names already reflect 026/027). Any other mismatch is a STOP
> condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW–MED — one migration drops two tables nothing writes to
- **Depends on**: plans/026-model-type-renames.md, plans/027-model-field-renames.md
- **Category**: tech-debt
- **Planned at**: commit `5c38943`, 2026-10-21

## Why this matters

`wuphf-model` carries types that are never used, or that duplicate another type
field for field. Each one is something a reader has to learn and rule out. This
plan removes seven of them without changing behavior:

1. `Contact` and `ChatSettings`, plus their storage methods and tables, have no
   caller outside the storage crate.
2. `RuleChat` has exactly the same fields as `ChatAddress`.
3. `ConnectorInfo` is `Connector` without the id.
4. `ConnectorInput` repeats two of `InboundWork`'s three variants.
5. `DispatchFailure` is an anonymous `(DispatchId, String)` tuple, while
   `ConnectorInput` already has a `DispatchFailed { dispatch_id, reason }` shape.
6. `IngestOutcome::Routed` carries a whole `RoutingOutcome` that only three
   tests read, and only for a count.
7. `NoticeTarget` is "a chat on a connector", which several other types also
   spell out field by field. It becomes a reusable `ChatRef`, which plan 031
   uses again for a conversation's source chat.

## Current state

Names below already reflect plans 026 and 027.

**1. Unused contacts and chat settings.**
`crates/wuphf-model/src/message.rs` defines:

```rust
pub struct Contact { pub id: String, pub first_name: Option<String>, pub full_name: Option<String>, pub push_name: Option<String>, pub business_name: Option<String> }
pub struct ChatSettings { pub endpoint_id: EndpointId, pub is_ephemeral: bool, pub ephemeral_timer_seconds: Option<u64> }
```

`crates/wuphf-storage/src/lib.rs` trait `Storage` has
`async fn put_contact(&self, contact: Contact)` and
`async fn put_chat_settings(&self, settings: ChatSettings)`. Both backends
implement them (`memory.rs`: `contacts` and `chat_settings` fields on `State`
plus the two methods; `sqlite.rs`: two methods using `upsert_json` on tables
`contacts` and `chat_settings`). The tables were created in
`migrations/0001_initial.sql`. Verified at planning time:
`grep -rn "put_contact\|put_chat_settings" crates` matches only the trait, the
two backends, and nothing else.

**2. `RuleChat` duplicates `ChatAddress`.** In `crates/wuphf-model/src/routing.rs`:

```rust
pub struct ChatAddress { pub chat_id: String, pub thread_id: Option<String> }   // derives Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize

pub struct RuleChat {
    pub chat_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}
impl RuleChat {
    pub fn matches(&self, addresses: &AddressSet) -> bool {
        addresses.iter().any(|address| self.names(address))
    }
    fn names(&self, address: &ChatAddress) -> bool {
        self.chat_id == address.chat_id
            && (self.thread_id.is_none() || self.thread_id == address.thread_id)
    }
    pub fn address(&self) -> ChatAddress {
        ChatAddress::new(self.chat_id.clone(), self.thread_id.clone())
    }
}
```

`RuleChat` is used in `RuleKind::OneToOne { source_chat, destination_chat }`,
`RuleKind::ManyToOne { destination_chat }`, and `ChatFilter::{Only, Not} { chats: Vec<RuleChat> }`.
`.address()` is called in `crates/wuphf-core/src/resolve.rs` (three places:
`source_chat.address()`, `destination_chat.address()`). `.matches(...)` is
called in `routing.rs` (`end_accepts`, `ChatFilter::accepts`).

**3. `ConnectorInfo` duplicates `Connector`.** `crates/wuphf-model/src/connector.rs`:

```rust
pub struct ConnectorInfo { pub platform: Platform, pub capabilities: ConnectorCapabilities }
pub struct Connector { pub id: ConnectorId, pub platform: Platform, #[serde(default)] pub capabilities: ConnectorCapabilities }
```

`crates/wuphf-router/src/lib.rs`:

```rust
pub struct RoutingConfig {
    rules: Vec<Rule>,
    connectors: HashMap<ConnectorId, ConnectorInfo>,
    notice_targets: Vec<NoticeTarget>,
}
impl RoutingConfig {
    pub fn new(rules: Vec<Rule>, connectors: HashMap<ConnectorId, ConnectorInfo>) -> Self { … }
    pub fn connector(&self, id: &ConnectorId) -> Option<&ConnectorInfo> { self.connectors.get(id) }
```

`crates/wuphf-cli/src/main.rs` (`serve`) builds both a
`HashMap<ConnectorId, ConnectorInfo>` and a `Vec<Connector>` (`allowed`) from
the same config loop. Tests build `ConnectorInfo` maps in
`crates/wuphf-core/src/tests.rs` (`fn connectors()`),
`crates/wuphf-runtime/src/tests.rs` (`fn connector_infos()`) and
`crates/wuphf-connector/tests/session.rs` (`fn routing()`).

**4. `ConnectorInput` repeats `InboundWork`.** `crates/wuphf-model/src/connector.rs`:

```rust
pub enum InboundWork {
    Event(Event),
    Migration { connector_id: ConnectorId, old: ChatAddress, new: ChatAddress },
    Notice { connector_id: ConnectorId, kind: NoticeKind, detail: String },
}

pub enum ConnectorInput {
    Event(Event),
    Acknowledgement(DispatchAck),
    IdentityMigrated { connector_id: ConnectorId, old: ChatAddress, new: ChatAddress },
    DispatchFailed { dispatch_id: DispatchId, reason: String },
    OwnerNotice { connector_id: ConnectorId, kind: NoticeKind, detail: String },
}
```

`crates/wuphf-runtime/src/connector.rs` (`ConnectorHub::accept_input`)
converts the three work variants into `InboundWork` by hand. For events it
overwrites `event.source.connector_id` with the session's id; for migrations
and notices it ignores the `connector_id` inside the input (`{ old, new, .. }`)
and uses the session's id. That "the session is the authority on which
connector spoke" rule must be preserved.

**5. The failure tuple.** `connector.rs`: `pub type DispatchFailure = (DispatchId, String);`
used by `RuntimeChannels::failures` and `Runtime::failure_rx` in
`crates/wuphf-runtime/src/lib.rs`; the hub sends
`self.runtime.failures.send((dispatch_id, reason))`.

**6. `IngestOutcome::Routed { message_id, routing: RoutingOutcome, dispatches }`**
in `crates/wuphf-model/src/dispatch.rs`. Production code never reads
`routing`. Tests read it at `crates/wuphf-core/src/tests.rs` around lines 553,
758 and 908, only as `routing.blocked.len()` and `routing.dispatches.is_empty()`.

**7. `NoticeTarget`.** `connector.rs`:
`pub struct NoticeTarget { pub connector_id: ConnectorId, pub chat_id: String, #[serde(default)] pub thread_id: Option<String> }`.
Used by `RoutingConfig` (`notice_targets`, `with_notice_targets`,
`notice_targets_excluding`), `crates/wuphf-core/src/notify.rs` (reads
`target.connector_id`, `target.chat_id`, `target.thread_id`),
`crates/wuphf-cli/src/main.rs` (`notify: Vec<NoticeTarget>` in the TOML
`Configuration`), and `crates/wuphf-core/src/tests.rs` around line 1667. The
TOML shape it parses is, from `config/example.toml`:

```toml
[[notify]]
connector_id = "telegram-main"
chat_id = "telegram:user:704338780"
```

That TOML shape must keep parsing unchanged.

Conventions: migrations are plain SQL files named `NNNN_snake_case.sql` in
`migrations/`, applied by `sqlx::migrate!` in `SqliteStorage::connect`, each
starting with a `--` comment that explains why (see
`migrations/0007_nullable_dispatch_message_id.sql`). The project instructions
(`AGENTS.md`) require: "SQLite schema changes require migrations".

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Build | `cargo build --workspace --all-targets` | exit 0 |
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| CLI config tests | `cargo test -p wuphf-cli` | exit 0 |
| All tests | `cargo test --workspace` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `crates/wuphf-model/src/{message,routing,connector,dispatch}.rs`
- `crates/wuphf-storage/src/{lib,memory,sqlite}.rs`
- `migrations/0011_drop_unused_tables.sql` (create)
- `crates/wuphf-router/src/lib.rs`
- `crates/wuphf-core/src/{resolve,notify,send,ingest,lib}.rs`, `crates/wuphf-core/src/tests.rs`
- `crates/wuphf-runtime/src/{lib,connector,grpc}.rs`, `crates/wuphf-runtime/src/tests.rs`
- `crates/wuphf-cli/src/main.rs`
- `crates/wuphf-connector/tests/session.rs`

**Out of scope** (do NOT touch):
- `crates/wuphf-proto/**`, `proto/go/**`, `connectors/**`: the wire frames do
  not change. `IdentityMigrated`, `OwnerNotice` and `DispatchFailed` stay as
  protobuf messages; only the Rust model behind them changes.
- `config/example.toml`: its format must not change.
- `RoutingOutcome` itself: plan 030 removes it. This plan only stops
  `IngestOutcome` from carrying it.
- `OwnerMessage` (`connector_id`, `chat_id`, `thread_id` fields): it could use
  `ChatRef` too, but that is a separate change.

## Git workflow

- Branch: `advisor/028-model-dead-and-duplicate-types`.
- One commit per step, e.g. `refactor(model): replace RuleChat with ChatAddress`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Remove contacts and chat settings

- Delete `Contact` and `ChatSettings` from `crates/wuphf-model/src/message.rs`.
- Delete `put_contact` and `put_chat_settings` from the `Storage` trait and
  from both backends; delete the `contacts` and `chat_settings` fields of
  `State` in `memory.rs`; remove them from the `use wuphf_model::{…}` lists.
- Create `migrations/0011_drop_unused_tables.sql`:

```sql
-- The contacts and chat_settings tables were created in 0001 for features that
-- never shipped; nothing has ever written to them. Dropping them keeps the
-- schema in step with the Storage trait.
DROP TABLE IF EXISTS contacts;
DROP TABLE IF EXISTS chat_settings;
```

**Verify**: `cargo test -p wuphf-storage` → exit 0 (its SQLite tests open a
fresh database, which runs every migration including 0011);
`grep -rn "Contact\b\|ChatSettings\|put_contact\|put_chat_settings" crates` → no matches.

### Step 2: Replace `RuleChat` with `ChatAddress`

- In `routing.rs`, add `#[serde(default)]` to `ChatAddress::thread_id` (so
  rule TOML that omits `thread_id` keeps parsing, as `RuleChat` allowed).
- Move the matching logic onto `ChatAddress`, renamed to say what it does:

```rust
impl ChatAddress {
    /// Whether this address, used as a rule's chat selector, names the chat
    /// known by any of `addresses`. Chat ids must match; a selector with no
    /// thread names the whole chat, one with a thread names only that thread.
    pub fn selects(&self, addresses: &AddressSet) -> bool {
        addresses.iter().any(|address| {
            self.chat_id == address.chat_id
                && (self.thread_id.is_none() || self.thread_id == address.thread_id)
        })
    }
}
```

- Delete `RuleChat`. Change every `RuleChat` field type to `ChatAddress`
  (`RuleKind` variants, `ChatFilter::{Only, Not}`), every `.matches(` on them
  to `.selects(`, and every `.address()` call on them in
  `crates/wuphf-core/src/resolve.rs` to `.clone()`.
- Tests that construct `RuleChat { chat_id, thread_id }` change to
  `ChatAddress { chat_id, thread_id }` (same fields).

**Verify**: `cargo test -p wuphf-cli` → exit 0 (both TOML parsing tests pass,
including the one-to-one rule with `thread_id = "12"` and the `kind = "not"`
filter); `cargo test -p wuphf-core` → exit 0;
`grep -rnw RuleChat crates` → no matches.

### Step 3: Replace `ConnectorInfo` with `Connector`

- Delete `ConnectorInfo` from `connector.rs`.
- In `crates/wuphf-router/src/lib.rs`: store `connectors: HashMap<ConnectorId, Connector>`;
  change the constructor to take a list and index it:

```rust
pub fn new(rules: Vec<Rule>, connectors: Vec<Connector>) -> Self {
    Self {
        rules,
        connectors: connectors
            .into_iter()
            .map(|connector| (connector.id.clone(), connector))
            .collect(),
        notice_targets: Vec::new(),
    }
}
pub fn connector(&self, id: &ConnectorId) -> Option<&Connector> { self.connectors.get(id) }
```

- Callers read only `.platform` (and capabilities where used); they compile
  unchanged against `Connector`.
- `crates/wuphf-cli/src/main.rs`: build one `Vec<Connector>` in the loop; pass
  `connectors.clone()` to `RoutingConfig::new` and `connectors` to
  `ConnectorHub::new`. Delete the separate `connector_infos` map.
- Test helpers (`crates/wuphf-core/src/tests.rs` `connectors()`,
  `crates/wuphf-runtime/src/tests.rs` `connector_infos()`,
  `crates/wuphf-connector/tests/session.rs` `routing()`): return
  `Vec<Connector>` with `id` set instead of a map. Where a test already has a
  `Vec<Connector>` of registrations for the same connectors, reuse it.

**Verify**: `cargo build --workspace --all-targets` → exit 0;
`cargo test --workspace` → exit 0; `grep -rnw ConnectorInfo crates` → no matches.

### Step 4: Make `ConnectorInput` carry `InboundWork` and a named failure

- In `connector.rs`, replace the type alias with a struct and reshape the enum:

```rust
/// A connector's report that a dispatch can never succeed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchFailed {
    pub dispatch_id: DispatchId,
    pub reason: String,
}

pub enum ConnectorInput {
    /// An event, identity migration or health notice, applied in connector order.
    Work(InboundWork),
    Acknowledgement(DispatchAck),
    DispatchFailed(DispatchFailed),
}
```

  Keep the existing doc comments, moving each variant's doc onto the matching
  `InboundWork` variant if it says more than the one already there. Add
  `#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]` to
  `InboundWork` if `ConnectorInput`'s derives require it.

- Add a method that applies the session's identity to any work item:

```rust
impl InboundWork {
    /// Stamp the connector the session belongs to. The session, not the wire,
    /// is the authority on which connector spoke.
    pub fn set_connector(&mut self, id: ConnectorId) {
        match self {
            InboundWork::Event(event) => event.source.connector_id = id,
            InboundWork::Migration { connector_id, .. }
            | InboundWork::Notice { connector_id, .. } => *connector_id = id,
        }
    }
}
```

- `crates/wuphf-runtime/src/grpc.rs` `input_from_proto`: return
  `ConnectorInput::Work(InboundWork::Event(…))` for the five event frames,
  `ConnectorInput::Work(InboundWork::Migration { … })` for `IdentityMigrated`,
  `ConnectorInput::Work(InboundWork::Notice { … })` for `OwnerNotice`, and
  `ConnectorInput::DispatchFailed(DispatchFailed { dispatch_id, reason })`.
- `crates/wuphf-runtime/src/connector.rs` `accept_input`: replace the `Event`,
  `IdentityMigrated` and `OwnerNotice` arms with one arm:

```rust
ConnectorInput::Work(mut work) => {
    work.set_connector(connector_id);
    let _ = self.runtime.inbound.send(work).await;
}
```

  The failure arm destructures `ConnectorInput::DispatchFailed(failure)`, does
  the same ownership check on `failure.dispatch_id`, and sends `failure`.
- `crates/wuphf-runtime/src/lib.rs`: `failure_rx: mpsc::Receiver<DispatchFailed>`,
  `RuntimeChannels::failures: mpsc::Sender<DispatchFailed>`; in `run`,
  `if let Some(DispatchFailed { dispatch_id, reason }) = failure`.
- Update `crates/wuphf-runtime/src/tests.rs`: `ConnectorInput::Event(e)`
  becomes `ConnectorInput::Work(InboundWork::Event(e))`, and failure sends use
  the struct.

**Verify**: `cargo test -p wuphf-runtime` → exit 0;
`grep -rnw "DispatchFailure\|IdentityMigrated {\|OwnerNotice {" crates --include='*.rs' | grep -v "pb::\|Body::"` → no matches.

### Step 5: Replace `IngestOutcome::Routed.routing` with a count

- In `dispatch.rs`: `Routed { message_id: MessageId, dispatches: Vec<Dispatch>, blocked: usize }`,
  with a doc comment on `blocked`: "How many dispatches were parked until
  another message is delivered first. They are not in `dispatches`; they are
  released when their parent is acknowledged."
- In `crates/wuphf-core/src/ingest.rs`, every `Routed { routing, … }` becomes
  `Routed { blocked: routing.blocked.len(), … }`. The replay branch in
  `ingest_create` (which used `routing: RoutingOutcome::default()`) uses
  `blocked: 0`.
- In `crates/wuphf-runtime/src/lib.rs` `ingest_event`, add `blocked` to the
  existing `debug!(… "event routed")` fields.
- Tests (`crates/wuphf-core/src/tests.rs` ~553, ~758, ~908):
  `routing.blocked.len()` → `blocked`; `routing.dispatches.is_empty()` →
  `dispatches.is_empty()`.

**Verify**: `cargo test -p wuphf-core` → exit 0;
`grep -rn "routing\." crates/wuphf-core/src/tests.rs` → no matches.

### Step 6: Replace `NoticeTarget` with a reusable `ChatRef`

- In `routing.rs`, next to `ChatAddress`:

```rust
/// A chat on a specific connector.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatRef {
    pub connector_id: ConnectorId,
    #[serde(flatten)]
    pub address: ChatAddress,
}
```

  `#[serde(flatten)]` keeps the TOML flat (`connector_id`, `chat_id`,
  `thread_id` side by side), exactly like `NoticeTarget`.
- Delete `NoticeTarget`; use `ChatRef` in `RoutingConfig`, in
  `crates/wuphf-cli/src/main.rs` (`notify: Vec<ChatRef>`), and in
  `crates/wuphf-core/src/notify.rs` (`target.chat_id` → `target.address.chat_id`,
  `target.thread_id` → `target.address.thread_id`). Update the test at
  `crates/wuphf-core/src/tests.rs` ~1667.
- In `crates/wuphf-cli/src/main.rs` test `example_configuration_parses`, add:

```rust
assert_eq!(configuration.notify.len(), 1);
assert_eq!(configuration.notify[0].connector_id, ConnectorId("telegram-main".into()));
assert_eq!(configuration.notify[0].address.chat_id, "telegram:user:704338780");
assert_eq!(configuration.notify[0].address.thread_id, None);
```

**Verify**: `cargo test -p wuphf-cli` → exit 0 including the new assertions;
`grep -rnw NoticeTarget crates` → no matches.

### Step 7: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` → exit 0.

## Test plan

- Migration 0011 is exercised by every SQLite test in
  `crates/wuphf-storage/src/tests.rs` (each opens a fresh database).
- `ChatAddress::selects` replaces `RuleChat::matches`; it is covered by the
  existing allowlist, blocklist and linked-address routing tests in
  `crates/wuphf-core/src/tests.rs`.
- New assertions in `example_configuration_parses` pin the `[[notify]]` TOML
  shape through the `flatten`.
- The runtime tests that send events, acknowledgements and failures through
  the hub cover the `ConnectorInput` reshaping; their pass/fail must not change.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings
- [ ] `grep -rnw "Contact\|ChatSettings\|RuleChat\|ConnectorInfo\|DispatchFailure\|NoticeTarget" crates` → no matches
- [ ] `migrations/0011_drop_unused_tables.sql` exists
- [ ] `git diff --stat -- crates/wuphf-proto proto connectors config` → empty
- [ ] `plans/README.md` status row updated

## STOP conditions

- `put_contact` or `put_chat_settings` has gained a caller outside
  `crates/wuphf-storage` since planning.
- `cargo test -p wuphf-cli` fails after Step 2 or Step 6. A config-format
  change is out of scope; report it rather than editing `config/example.toml`.
- Any runtime test changes outcome after Step 4. The hub must still reject an
  acknowledgement or failure for a dispatch not assigned to that session, and
  must still stamp the session's connector id on every work item.
- A migration numbered `0011` already exists (another plan landed one); use
  the next free number and note it in the commit message.

## Maintenance notes

- Plan 021 (storage role traits) also proposed deleting `put_contact` and
  `put_chat_settings`. This plan does it; 021 must be refreshed before it runs.
- `ChatRef` is introduced here so plan 031 can use it for a conversation's
  source chat. `OwnerMessage`, `Alias`, `AddressLink`, `ObservedMessage` and
  `SourceAddress` still spell connector-plus-chat out by hand; converting them
  is optional follow-up.
- Reviewer focus: the hub's identity stamping (Step 4) and the unchanged TOML
  formats (Steps 2 and 6).
