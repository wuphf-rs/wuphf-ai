# Plan 026: Rename the wuphf-model types to plain names (type names only)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 5c38943..HEAD -- crates/ docs/architecture.md docs/index.html README.md`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. Renames from plan 019 landing first
> are expected (019 is meant to land before this plan) and are not drift.

## Status

- **Priority**: P2
- **Effort**: M (mechanical, wide)
- **Risk**: LOW — type names never appear in stored JSON or on the wire
- **Depends on**: plans/019-settlement-single-source.md (soft: 019 quotes the
  current type names in its excerpts, so land 019 first to spare it a drift STOP)
- **Category**: tech-debt
- **Planned at**: commit `5c38943`, 2026-10-21

## Why this matters

The owner of this repo finds the type system hard to read. Many names describe
how a value is built or stored rather than what it is (`StoredDispatch`,
`NormalizedEvent`, `EventPayload`, `DispatchPayload`, `ConnectorRegistration`),
or misdescribe it (`PlatformAddress` addresses a chat, not a platform; "raw
send" means "a message the owner typed"). This plan renames **Rust type names
only**. It deliberately does not rename struct fields or enum variants: those
appear in JSON stored in SQLite and are handled with serde aliases in plan 027.
Because Rust type names are never serialized, this plan cannot break stored
data or the gRPC wire contract.

## Current state

All types live in `crates/wuphf-model/src/` and are re-exported flatly from
`crates/wuphf-model/src/lib.rs` (`pub use connector::*; pub use dispatch::*;`
…), so every crate refers to them as `wuphf_model::Name`.

The renames, with where each type is defined today:

| Old name | New name | Defined in |
|---|---|---|
| `PlatformAddress` | `ChatAddress` | `routing.rs` (`pub struct PlatformAddress { pub chat_id: String, pub thread_id: Option<String> }`) |
| `StoredDispatch` | `Dispatch` | `dispatch.rs` (`pub struct StoredDispatch { id, command, state }`) |
| `DispatchPayload` | `Action` | `dispatch.rs` (`pub enum DispatchPayload { MessageCreate {..}, … }`) |
| `DispatchAcknowledgement` | `DispatchAck` | `dispatch.rs` |
| `NormalizedEvent` | `Event` | `message.rs` (`pub struct NormalizedEvent { source, payload }`) |
| `EventPayload` | `EventKind` | `message.rs` |
| `InboundMessage` | `NewMessage` | `message.rs` |
| `ConnectorRegistration` | `Connector` | `connector.rs` |
| `RoutingRuleFlow` | `RuleKind` | `routing.rs` |
| `RoutingRule` | `Rule` | `routing.rs` |
| `RuleChatsFilter` | `ChatFilter` | `routing.rs` |
| `RawOutbound` | `OwnerMessage` | `dispatch.rs` |
| `RawSendOutcome` | `OwnerSendOutcome` | `dispatch.rs` |
| `RAW_SENDER` (const) | `OWNER_AUTHOR` | `dispatch.rs` (`pub const RAW_SENDER: &str = "me";`) |

Related non-model names that use the same "raw" wording, renamed in Step 4:

| Old | New | Where |
|---|---|---|
| `RawMessageSender` | `OwnerMessageSender` | `crates/wuphf-runtime/src/raw_send.rs`, re-exported from `crates/wuphf-runtime/src/lib.rs:22` |
| `RawSendError` | `OwnerSendError` | same file |
| module `raw_send` | module `owner_send` | file `crates/wuphf-runtime/src/raw_send.rs` → `owner_send.rs` |
| `Core::send_raw` | `Core::send_owner_message` | `crates/wuphf-core/src/send.rs:30` (`pub async fn send_raw(&self, outbound: RawOutbound)`) |
| `Runtime::raw_sender` | `Runtime::owner_sender` | `crates/wuphf-runtime/src/lib.rs:84` |

Facts verified at planning time:

- None of the new names collide with an existing type in the crates that
  import `wuphf_model`. The only textual matches are enum *variants*
  (`ConnectorInput::Event`, `ConnectorOutput::Dispatch`), which can coexist
  with types of the same name. `wuphf-tui` has its own `Action` and uses
  crossterm's `Event`, but it does not depend on `wuphf-model`, so it is
  unaffected.
- JSON is produced only in `crates/wuphf-storage` (and tests). serde writes
  struct field names and enum variant names, never Rust type names, so
  renaming types changes no stored bytes.
- The protobuf types (`wuphf_proto::connector::*`, used as `pb::…`) are a
  separate type system. Names like `pb::MessageCreateEvent`,
  `pb::Registration` must NOT change.

Conventions: doc comments explain intent in full sentences (see any item in
`crates/wuphf-model/src/dispatch.rs`); keep comments accurate after a rename
(e.g. a doc that says "a [`StoredDispatch`]" must become "a [`Dispatch`]").
Commit style is conventional commits, e.g. `refactor(core): …` (see
`git log --oneline -20`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Format check | `cargo fmt --check` | exit 0 |
| Build everything | `cargo build --workspace --all-targets` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Tests | `cargo test --workspace` | exit 0, same passed count as the Step 0 baseline |
| Word-boundary replace (GNU sed) | `grep -rlw OLD crates docs README.md \| xargs sed -i 's/\bOLD\b/NEW/g'` | files rewritten |

The build needs `protoc` on PATH (the repo's nix dev shell provides it:
`nix develop`). On macOS without GNU sed, use
`perl -pi -e 's/\bOLD\b/NEW/g'` instead of `sed -i`.

## Scope

**In scope**:
- `crates/wuphf-model/src/**`
- Every `*.rs` file under `crates/` that refers to a renamed name (model
  users: `wuphf-storage`, `wuphf-router`, `wuphf-core`, `wuphf-runtime`,
  `wuphf-cli`, `crates/wuphf-connector/tests/session.rs`)
- `crates/wuphf-runtime/src/raw_send.rs` → renamed to `owner_send.rs`
- `docs/architecture.md`, `docs/index.html` — only where they spell a renamed
  type name (e.g. `NormalizedEvent`, `RoutingRule`)

**Out of scope** (do NOT touch):
- `crates/wuphf-proto/**`, `proto/go/**`, `connectors/**` — the wire contract
  and Go/Rust connectors keep their names.
- Struct field names and enum variant names anywhere (for example
  `platform_message_id`, `sender`, `MessageCreate`, `ReactionSet`) — plan 027.
- `crates/wuphf-tui/**` — does not use the model crate.
- `migrations/**` — no schema change.
- `config/example.toml` — the TOML format does not change.
- Any behavior change. Every edit in this plan is a rename.

## Git workflow

- Branch: `advisor/026-model-type-renames` off `v2`.
- One commit per step, e.g. `refactor(model): rename PlatformAddress to ChatAddress`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 0: Record the baseline

Run `cargo test --workspace 2>&1 | grep "^test result"` and write down the sum
of the `passed` counts. Every later "same passed count" check compares to it.

**Verify**: `cargo test --workspace` → exit 0.

### Step 1: Rename the address and dispatch types

Apply word-boundary replacements across `crates/` (all `.rs` files), in this
order:

1. `PlatformAddress` → `ChatAddress`
2. `StoredDispatch` → `Dispatch`
3. `DispatchPayload` → `Action`
4. `DispatchAcknowledgement` → `DispatchAck`

Word boundaries matter: `\bStoredDispatch\b` must not touch
`ConnectorOutput::Dispatch`. After replacing, read the diff of
`crates/wuphf-model/src/dispatch.rs` and fix any doc comment that now reads
oddly (for example "the persisted dispatch" wording is fine; a sentence that
named the old type is now auto-updated).

**Verify**: `cargo build --workspace --all-targets` → exit 0, and
`grep -rnw "PlatformAddress\|StoredDispatch\|DispatchPayload\|DispatchAcknowledgement" crates` → no matches.

### Step 2: Rename the event types

Replace, in this order: `NormalizedEvent` → `Event`, `EventPayload` →
`EventKind`, `InboundMessage` → `NewMessage`.

Check for an import clash after replacing: any file that now has both
`use wuphf_model::{… Event …}` and another `Event` in scope must fail to
compile; none is expected (verified at planning time). If one appears, STOP.

**Verify**: `cargo build --workspace --all-targets` → exit 0, and
`grep -rnw "NormalizedEvent\|EventPayload\|InboundMessage" crates` → no matches.

### Step 3: Rename the connector and rule types

Replace, in this order (order matters: `RoutingRuleFlow` before
`RoutingRule`): `RoutingRuleFlow` → `RuleKind`, `RoutingRule` → `Rule`,
`RuleChatsFilter` → `ChatFilter`, `ConnectorRegistration` → `Connector`.

`wuphf-cli` keeps its own private `ConnectorConfiguration` struct (the TOML
shape); do not rename it. The TOML keys (`kind = "many-to-one"`,
`source_chats_filter`, …) come from field and variant names and must not change.

**Verify**: `cargo build --workspace --all-targets` → exit 0;
`cargo test -p wuphf-cli` → exit 0 (its two tests parse the example config and
an inline TOML rule set, proving the config format is unchanged).

### Step 4: Rename the "raw send" vocabulary to "owner message"

- Model: `RawOutbound` → `OwnerMessage`, `RawSendOutcome` → `OwnerSendOutcome`,
  `RAW_SENDER` → `OWNER_AUTHOR`.
- Core: rename `send_raw` → `send_owner_message` (definition in
  `crates/wuphf-core/src/send.rs`, callers in `crates/wuphf-core/src/notify.rs`,
  `crates/wuphf-runtime/src/raw_send.rs` and tests). Update the module doc at
  the top of `send.rs` ("Raw outbound sending: …") to say "Owner messages: …".
- Runtime: `git mv crates/wuphf-runtime/src/raw_send.rs crates/wuphf-runtime/src/owner_send.rs`;
  in `crates/wuphf-runtime/src/lib.rs` change `pub mod raw_send;` to
  `pub mod owner_send;` and the re-export to
  `pub use owner_send::{OwnerMessageSender, OwnerSendError};`; rename
  `RawMessageSender` → `OwnerMessageSender`, `RawSendError` → `OwnerSendError`,
  `Runtime::raw_sender` → `Runtime::owner_sender` and every use (including
  `crates/wuphf-cli/src/main.rs`, which calls `runtime.raw_sender()`, and
  `crates/wuphf-runtime/src/observer_service.rs`, which imports
  `crate::raw_send::{self, RawMessageSender}` and calls `raw_send::now_ms()`).

The value of `OWNER_AUTHOR` stays `"me"` — it is stored inside message JSON
as the author of owner messages.

**Verify**: `cargo build --workspace --all-targets` → exit 0;
`grep -rnw "RawOutbound\|RawSendOutcome\|RAW_SENDER\|RawMessageSender\|RawSendError\|send_raw\|raw_send\|raw_sender" crates` → no matches.

### Step 5: Update the docs that spell type names

In `docs/architecture.md` and `docs/index.html`, replace the old type names
from the tables above where they appear as code (for example the data-flow
line `Connector --NormalizedEvent (raw address)--> …` becomes
`Connector --Event (raw address)--> …`; "A `RoutingRule` declares a flow"
becomes "A `Rule` declares a flow"). `DispatchCommand` is NOT renamed in this
plan; leave it. Do not reword anything else.

**Verify**: `grep -nw "NormalizedEvent\|RoutingRule\|PlatformAddress\|StoredDispatch" docs/architecture.md docs/index.html` → no matches.

### Step 6: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` → exit 0, passed count equals the Step 0 baseline.

## Test plan

No new tests: this plan changes no behavior, and the compiler checks every
rename. The existing suite (core, runtime, storage, CLI config parsing, and the
connector session test) is the regression net and must pass with the same
count as Step 0.

## Done criteria

- [ ] `cargo fmt --check` exits 0
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings
- [ ] `cargo test --workspace` exits 0 with the Step 0 passed count
- [ ] `grep -rnw "PlatformAddress\|StoredDispatch\|DispatchPayload\|DispatchAcknowledgement\|NormalizedEvent\|EventPayload\|InboundMessage\|RoutingRuleFlow\|RoutingRule\|RuleChatsFilter\|ConnectorRegistration\|RawOutbound\|RawSendOutcome\|RAW_SENDER\|RawMessageSender\|RawSendError" crates` → no matches
- [ ] `git diff --stat -- crates/wuphf-proto proto connectors migrations config crates/wuphf-tui` → empty
- [ ] `plans/README.md` status row updated

## STOP conditions

- The drift check shows in-scope files changed in ways other than plan 019's
  storage edits, and the excerpts above no longer match.
- A rename produces a name clash the compiler reports (two items named
  `Event`, `Action`, `Dispatch`, `Connector` or `Rule` in one scope).
- Any test fails after a step. A pure rename cannot change behavior, so a
  failure means something other than a rename happened.
- A replacement would have to touch a file in `crates/wuphf-proto`,
  `proto/go` or `connectors/`.

## Maintenance notes

- Plan 027 renames the fields and variants of these types, with serde aliases
  so existing SQLite rows still load. Plans 028–031 build on these names.
- Reviewer focus: the diff should be renames only. Any changed expression,
  added line of logic, or changed string literal (other than doc comments) is
  out of scope.
- Plans written before this one (003, 007, 017, 021, 022) quote the old names
  in their excerpts. Their drift checks will flag that; the mapping table above
  translates them.
