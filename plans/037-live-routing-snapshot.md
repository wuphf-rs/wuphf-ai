# Plan 037: Route from a live settings snapshot; make `rules` and `notify` runtime keys

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-core/src crates/wuphf-settings crates/wuphf-cli/src/main.rs`
> Plans 035 and 036 must have landed (`wuphf_settings::{WuphfSchema, SettingsStore, SettingsStorage}`
> exist and `serve()` loads the store over SQLite); if not, STOP. Then check
> that every `self.config` / `self.router` use in `crates/wuphf-core/src`
> listed under "Current state" is still where it says (`grep -n "self.config\|self.router" crates/wuphf-core/src/*.rs`);
> new uses are fine (handle them the same way), a changed `Core::new`
> signature is a STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED — touches how every event reaches the routing config; the
  routing *logic* is unchanged
- **Depends on**: 035, 036
- **Category**: direction (runtime settings, step 3 of wuphf's part)
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

The owner wants to change which chats mirror each other, and where connector
notices go, without restarting the core: first from the `settings` CLI (plan
042), then from a panel on Telegram or Discord (plans 039–041). Today the
core gets one `Arc<RoutingConfig>` at startup and keeps it forever. This plan
makes the core read the routing config from a source that the settings store
keeps current, so a validated change applies to the next event, and declares
`rules` and `notify` as the first runtime keys.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The core stays authoritative and does not depend on figment or botconf. It reads routing through a one-method trait, `RoutingSource`, defined in `wuphf-core`; `wuphf-settings` implements it over the settings store. |
| 2 | `rules` and `notify` become runtime keys. Everything else stays file/environment only — in particular `connectors`, `listen`, `database_url`, the tokens and `functions`. The `ConnectorHub`'s connector list stays fixed at startup. |
| 3 | A change is validated like the file: by rebuilding the whole configuration and running `derive`. An invalid change is rejected and never stored. |
| 4 | One event sees one routing config: each core entry point reads the source once and uses that snapshot for the whole operation. |
| 5 | Keys are `Kind::Json` for now. `notify` moves to botconf's `Kind::Ref` once botconf plan 003 lands; `rules` becomes a per-entry map in plan 038. |

### Decisions this plan makes (confirmed by the owner 2026-10-25)

- **Validation moves into the core.** `RoutingConfig::validate()` (new, in
  `wuphf-core`) rejects rules naming an unconfigured connector, duplicate rule
  ids, and notice targets on an unconfigured connector. Today nothing
  validates rules (plan 003 targeted an older rule shape and is stale), so a
  bad file used to start and misbehave later; now it fails at startup with
  the reason. This is a startup behaviour change for invalid configs only.
- **Capability gating stays as it is.** The example config says
  `supports_threads`/`supports_topic_create` gate which rule ends a connector
  may serve, but `wuphf-core` does not check them when building routing
  (`grep -n supports_threads crates/wuphf-core/src/*.rs` finds only tests).
  Adding that check is a behaviour change outside this plan; it is listed in
  Maintenance notes.

## Current state

- `crates/wuphf-core/src/lib.rs:22-61` — the core keeps the config and a
  router built from it:

  ```rust
  pub struct Core<S> {
      storage: Arc<S>,
      router: Router<S>,
      config: Arc<RoutingConfig>,
      provisioning: Mutex<HashMap<ConversationId, ProvisionState>>,
      notices: Mutex<HashMap<ConnectorId, NoticeKind>>,
  }
  pub fn new(storage: Arc<S>, config: Arc<RoutingConfig>) -> Self {
      Self {
          router: Router::new(Arc::clone(&storage), Arc::clone(&config)),
          config, storage, /* … */
      }
  }
  ```

- `crates/wuphf-core/src/router.rs:15-26` — `Router { storage, config:
  Arc<RoutingConfig> }`; `self.config.rule(rule_id)` at line 294 (`is_mirror`).
- Other reads of the config (all `self.config`):
  `lib.rs:72` (`connector_platform`), `send.rs:113` (`owner_message_endpoint`),
  `notify.rs:37` (`notify_owners`: `notice_targets_excluding`),
  `resolve.rs:38` (`resolve_origin`: `match_source`), `resolve.rs:106`
  (`still_covered`: a conversation whose rule is gone stops replicating —
  that behaviour is what makes rule *removal* safe at runtime),
  `resolve.rs:252` (`ensure_end_endpoint`: connector lookup).
- Public entry points (`pub async fn` / `pub fn` on `Core`): `ingest`
  (lib.rs:77), `record_dispatched_copy`, `acknowledge_dispatch`,
  `recover_provisioning`, `migrate_identity`, `pending_dispatches`,
  `fail_dispatch` (acknowledge.rs), `notify_owners` (notify.rs:25),
  `send_owner_message` (send.rs:30), `connector_platform` (lib.rs:71).
- `crates/wuphf-core/src/config.rs` — `RoutingConfig { rules, connectors,
  notice_targets }` with `new`, `with_notice_targets`,
  `notice_targets_excluding`, `rule`, `connector`, `match_source`. No
  validation.
- `Core::new` callers: `crates/wuphf-cli/src/main.rs` (serve),
  `crates/wuphf-core/src/tests.rs:77, 1655`,
  `crates/wuphf-runtime/src/tests.rs:73, 100`,
  `crates/wuphf-connector/tests/session.rs:92` — all pass an
  `Arc<RoutingConfig>`. Core tests build configs with `fn core(rules)`
  (tests.rs:68-78) and `connectors()`.
- `crates/wuphf-settings` (from 035/036) — `WuphfSchema::settings()` returns
  an empty list; `Derived { routing: Arc<RoutingConfig>, connectors, functions }`.
- Rule ids are persisted: `conversations.rule_id` (migration 0014) is how a
  conversation remembers its rule. Removing a rule therefore stops its
  conversations (`still_covered`); re-adding a rule with the same id resumes
  them; a *renamed* rule is a new rule (new conversations, new mirror threads).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -p wuphf-core` | exit 0 |
| Settings tests | `cargo test -p wuphf-settings` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline + new tests |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| No stale reads | `grep -n "self\.config\|self\.router" crates/wuphf-core/src/*.rs \| grep -v tests.rs` | no output |
| Boundary | `cargo tree -p wuphf-core --depth 1 \| grep -E "figment\|botconf"` | no output |

## Scope

**In scope**: `crates/wuphf-core/src/{lib.rs,config.rs,router.rs,resolve.rs,send.rs,notify.rs,ingest.rs,acknowledge.rs,tests.rs}`;
`crates/wuphf-settings/src/{schema.rs,lib.rs,routing.rs (create),tests.rs}`;
`crates/wuphf-cli/src/main.rs`; `config/example.toml` (comments);
`docs/architecture.md` (one paragraph).

**Out of scope**: routing logic (`match_source`, filters, fan-out, provisioning);
`ConnectorHub` and the connector list; `functions`; the rules TOML shape
(plan 038); any front end (plans 039–042); capability gating.

## Git workflow

- Commit on the current branch. Do not create a branch, push, or open a PR
  unless the operator instructed it.
- Commits:
  1. `feat(core): validate routing config`
  2. `refactor(core): read routing through a RoutingSource once per operation`
  3. `feat(settings): make rules and notify runtime settings`
  4. `docs: describe runtime routing changes`

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ record it.

### Step 1: `RoutingConfig::validate`

In `crates/wuphf-core/src/config.rs` add a `thiserror` enum and:

```rust
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoutingConfigError {
    #[error("rule {rule} names connector {connector}, which is not configured")]
    UnknownRuleConnector { rule: RuleId, connector: ConnectorId },
    #[error("rule id {0} is used more than once")]
    DuplicateRule(RuleId),
    #[error("notice target names connector {0}, which is not configured")]
    UnknownNoticeConnector(ConnectorId),
}

impl RoutingConfig {
    /// Checks what serde cannot: every rule and notice target names a
    /// configured connector, and rule ids are unique.
    pub fn validate(&self) -> Result<(), RoutingConfigError>;
}
```

(The connectors a rule names are `source_connector` and
`destination_connector` of either `RuleKind` variant; add a
`RuleKind::connectors(&self) -> [&ConnectorId; 2]` helper in `wuphf-model`
only if a match in `validate` would be duplicated — otherwise match inline.)
Re-export `RoutingConfigError` from `lib.rs`.

In `wuphf-settings`'s `derive`, call `routing.validate()` after building it
and return its message on error.

Tests in `crates/wuphf-core/src/tests.rs`: `validate_rejects_unknown_rule_connector`,
`validate_rejects_duplicate_rule_ids`, `validate_rejects_unknown_notice_connector`,
`validate_accepts_the_test_fixture` (the `connectors()` + a one-to-one and a
many-to-one rule used elsewhere in the file). In `wuphf-settings`:
`example_configuration_validates` (load the example; no error).

**Verify**: `cargo test -p wuphf-core -p wuphf-settings` → all pass.

Commit 1.

### Step 2: `RoutingSource`, read once per operation

In `crates/wuphf-core/src/config.rs`:

```rust
/// Where the core gets its routing config. Read once at the start of each
/// operation, so one event is routed against one consistent config even if
/// it changes meanwhile.
pub trait RoutingSource: Send + Sync + 'static {
    fn routing(&self) -> Arc<RoutingConfig>;
}

/// A fixed config, e.g. in tests.
impl RoutingSource for Arc<RoutingConfig> {
    fn routing(&self) -> Arc<RoutingConfig> {
        Arc::clone(self)
    }
}
```

Re-export it from `lib.rs`.

`Core`:

- Replace the `router` and `config` fields with
  `routing: Arc<dyn RoutingSource>`.
- `pub fn new(storage: Arc<S>, routing: impl RoutingSource) -> Self` — every
  existing caller passes an `Arc<RoutingConfig>` and compiles unchanged.
- Add `fn router(&self, routing: &Arc<RoutingConfig>) -> Router<S>` that
  builds a router for one operation (two `Arc` clones; `Router` stays as is).
- In each public entry point that needs routing (directly or through a
  helper), read once at the top: `let routing = self.routing.routing();` and
  pass `&routing` (or `&RoutingConfig`) down. Change every private helper that
  used `self.config` or `self.router` to take the config as a parameter:
  `resolve_origin`, `still_covered`, `ensure_conversation`,
  `ensure_end_endpoint`, `owner_message_endpoint`, `fan_out_to_mirrors`,
  `route_and_record`, and the `ingest_*` functions that call the router. Entry
  points that never touch routing (`pending_dispatches`,
  `record_dispatched_copy` if it doesn't) do not read it — check each by
  following its calls, and do not read "just in case".
- `connector_platform` reads the source too (the connector list cannot
  change at runtime, but there is no reason to keep a second copy).

**Verify**: `cargo build --workspace --all-targets` → exit 0; the
"No stale reads" grep → no output; `cargo test --workspace` → all existing
tests pass unchanged.

Add tests in `crates/wuphf-core/src/tests.rs`, with two test sources defined
in the test module:

```rust
/// A routing source whose config the test can swap.
struct Swappable(std::sync::Mutex<Arc<RoutingConfig>>);
/// Counts reads, to check each operation reads exactly once.
struct Counting { inner: Arc<RoutingConfig>, reads: AtomicUsize }
```

1. `rule_added_at_runtime_routes_the_next_event` — core over `Swappable` with
   no rules; ingest a create from the source chat → `Ignored(Unmatched)`;
   swap in a config with a one-to-one rule for that chat; ingest the next
   create → a dispatch to the destination.
2. `rule_removed_at_runtime_stops_the_conversation` — rule present, ingest
   (conversation created, dispatch out); swap to a config without the rule;
   the next create from the same chat produces no dispatch.
3. `notice_targets_follow_runtime_changes` — swap notice targets between two
   chats; `notify_owners` dispatches to the new one.
4. `each_operation_reads_routing_once` — over `Counting`: one routed
   `ingest`, one `acknowledge_dispatch`, one `notify_owners`, one
   `send_owner_message`; after each, `reads` grew by exactly 1. (If an entry
   point legitimately needs no routing, assert 0 for it and say why in a
   comment.)

Temporarily make one helper call `self.routing.routing()` again instead of
using its parameter, and confirm test 4 fails; restore.

**Verify**: `cargo test -p wuphf-core` → all pass.

Commit 2.

### Step 3: Runtime keys and the live source

In `crates/wuphf-settings/src/schema.rs`, `settings()` returns:

```rust
vec![
    RuntimeSetting::new(
        "rules",
        "Routing rules: which chats mirror each other (see config/example.toml)",
    )
    .titled("Rules")
    .kind(Kind::Json),
    RuntimeSetting::new(
        "notify",
        "Chats that receive notices about connector trouble",
    )
    .titled("Notice targets")
    .kind(Kind::Json),
]
```

New `crates/wuphf-settings/src/routing.rs`:

```rust
/// The routing config of the settings store's current snapshot.
pub struct LiveRouting(Arc<SettingsStore>);

impl RoutingSource for LiveRouting {
    fn routing(&self) -> Arc<RoutingConfig> {
        Arc::clone(&self.0.current().derived.routing)
    }
}
```

(`SettingsStore::current()` is an `ArcSwap` load — cheap, lock-free.)

In `serve()`: `Core::new(Arc::clone(&storage), LiveRouting::new(Arc::clone(&settings)))`.
The `ConnectorHub` still gets `snapshot.derived.connectors` once at startup.

Tests in `crates/wuphf-settings/src/tests.rs` (store over a temp-file SQLite
through `SettingsStorage`, as in plan 036):

- `rules_set_at_runtime_reach_live_routing` — `store.set("rules", json!([...one rule...]), None)`;
  `LiveRouting::new(store).routing().rule(&id)` is `Some`.
- `invalid_rules_are_rejected_and_not_stored` — a rule naming an unknown
  connector → `SettingsError::InvalidValue`; current routing unchanged; the
  settings table is empty.
- `only_rules_and_notify_change_at_runtime` — `set` on `connectors`,
  `listen`, `database_url`, `functions` → `SettingsError::UnknownKey`.
- `rules_override_survives_a_restart` — set, reopen the database, load a new
  store: the rule is there and `source("rules") == Source::Stored`. Replace
  plan 036's placeholder `sqlite_overrides_survive_a_restart` with this (it
  relied on `rules` being unknown).
- `unset_rules_falls_back_to_the_file`.

**Verify**: `cargo test --workspace` → all pass.

Commit 3.

### Step 4: Document

- `config/example.toml`: above `[[notify]]` and above the rules section, one
  comment line each: "Runtime setting: can also be changed while the core
  runs (stored overrides win over this file; unsetting one goes back to this
  file)."
- `docs/architecture.md`: a short "Runtime settings" paragraph — the store,
  which keys are runtime, that one operation uses one routing snapshot, and
  the rule-id consequences from "Current state" (removing a rule stops its
  conversations; renaming one starts new ones).

Commit 4.

### Step 5: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0; the "No stale reads" and "Boundary" commands → no output.

## Test plan

- `wuphf-core`: `validate_rejects_unknown_rule_connector`,
  `validate_rejects_duplicate_rule_ids`,
  `validate_rejects_unknown_notice_connector`,
  `validate_accepts_the_test_fixture`,
  `rule_added_at_runtime_routes_the_next_event`,
  `rule_removed_at_runtime_stops_the_conversation`,
  `notice_targets_follow_runtime_changes`, `each_operation_reads_routing_once`.
- `wuphf-settings`: `example_configuration_validates`,
  `rules_set_at_runtime_reach_live_routing`,
  `invalid_rules_are_rejected_and_not_stored`,
  `only_rules_and_notify_change_at_runtime`,
  `rules_override_survives_a_restart`, `unset_rules_falls_back_to_the_file`.

## Done criteria

- [ ] `cargo test --workspace` exits 0; the tests above exist by name
- [ ] clippy and fmt clean; "No stale reads" and "Boundary" commands print nothing
- [ ] Every pre-existing `Core::new(storage, Arc<RoutingConfig>)` call compiles unchanged
- [ ] `git diff --stat 643829b..HEAD` lists only in-scope paths for this plan
- [ ] `plans/README.md` status row updated (and note on plan 003's row that
      routing validation landed here)

## STOP conditions

- An entry point turns out to need routing at two different moments of one
  operation (e.g. before and after an `await` that must observe a change) —
  report it rather than reading twice.
- Validation rejects `config/example.toml` or a fixture used by existing
  tests (it means the fixture is wrong or the rule is too strict — report).
- botconf's `RuntimeSetting`/`Kind` API differs from the excerpt above.

## Maintenance notes

- Editing a rule in place (same id, new destination) does not move existing
  conversations: their endpoints are already stored and are reused. To re-home
  a mirror, add the rule under a new id and remove the old one (new mirror
  threads), or a later plan can add explicit migration. Surface this in the
  panel's rule description when plan 041 builds it.
- Capability gating (`supports_threads`/`supports_topic_create` for
  many-to-one destinations) belongs in `RoutingConfig::validate` once the
  owner wants it enforced.
- Plan 038 rewrites `notify` and rule chats as `connector:chat#thread`
  strings (migrating any override this plan stored), and plan 039 makes them
  `wuphf.chat` references, so the panel shows chat names and picks chats
  instead of taking JSON.
