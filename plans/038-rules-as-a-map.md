# Plan 038: Key routing rules by id (`[rules.<id>]`) so each rule is its own setting

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-settings crates/wuphf-model/src/routing.rs crates/wuphf-core/src/config.rs config/example.toml migrations`
> Plan 037 must have landed (`rules` is a runtime key, `RoutingConfig::validate`
> exists); if not, STOP. Find the next free migration number with
> `ls migrations/` (expected `0016`, after plan 036's `0015_settings.sql`) and
> use it wherever this plan says `0016`.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW–MED — changes the config file format for rules (every config
  with rules must be rewritten); routing behaviour is unchanged
- **Depends on**: 037
- **Category**: direction (runtime settings, step 4 of wuphf's part)
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

After plan 037, `rules` is one runtime setting holding a JSON list: changing
any rule from a panel means re-sending every rule, and two people editing
different rules overwrite each other. botconf supports **per-entry** map
settings: a map whose entries are set and removed one at a time
(`rules.<id>`), each listed as its own row in the settings panel. Rules
already have unique, user-chosen ids, so the natural shape is a map keyed by
id: `[rules.wa-to-telegram-mirror]` instead of `[[rules]]` with an `id`
field. This plan changes the file format and the runtime key to that shape.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Rules are a map keyed by rule id, in the file (`[rules.<id>]`) and as a per-entry runtime setting (`rules.<id>` can be set or removed on its own; the panel lists each rule as an entry). |
| 2 | Rules live in the config file and the settings table, not in the message database, so the format change itself needs no migration of message data. |

### Decisions this plan makes (flag for the owner)

- **The id comes only from the key.** An entry has no `id` field; writing one
  is an error (unknown field). The `wuphf_model::Rule { id, flow }` type is
  unchanged — the settings layer builds it from the key — so the core, the
  storage and every test that builds `Rule` values stay as they are.
- **The flow is flattened into the entry.** No nested `[rules.<id>.flow]`
  table: `kind = "many-to-one"` and the flow's fields sit directly under
  `[rules.<id>]`. One level less to type, and a per-entry value from the panel
  is a single flat object.
- **Order: an optional `priority` (default 0), then the id.** Rule order
  matters today: `RoutingConfig::match_source` returns the *first* matching
  rule end, so a one-to-one exception listed before a many-to-one catch-all
  wins. A map has no order (figment's maps are sorted by key, and so is a
  JSON object stored by botconf), so order must be explicit. Lower priority
  numbers match first; ties go by id. Configs without overlapping rules never
  need to set it.
- **Rule ids must be valid entry names**: non-empty and without `.` (botconf
  addresses an entry as `rules.<id>`). Checked in `derive`.

## Current state

- `crates/wuphf-model/src/routing.rs:157-187` — the model:

  ```rust
  pub struct Rule { pub id: RuleId, pub flow: RuleKind }

  #[serde(rename_all = "kebab-case", tag = "kind")]
  pub enum RuleKind {
      OneToOne { source_connector, source_chat, destination_connector, destination_chat },
      ManyToOne { source_connector, #[serde(default)] source_chats_filter: ChatFilter,
                  destination_connector, destination_chat },
  }
  ```

  `ChatFilter` is also internally tagged (`kind`, lowercase).
  `RuleId(pub String)` is in `crates/wuphf-model/src/ids.rs`.
- `crates/wuphf-core/src/config.rs` — `RoutingConfig::match_source`
  iterates `self.rules` in order and returns the first accepting end; this is
  why order matters.
- `crates/wuphf-settings/src/config.rs` (from 035) — `Configuration.rules:
  Vec<Rule>` (`#[serde(default)]`); `derive` passes it to `RoutingConfig::new`.
  `schema.rs` (from 037) declares `RuntimeSetting::new("rules", …).kind(Kind::Json)`.
- Rule ids in SQLite: `conversations.rule_id` (`migrations/0014_merge_conversations.sql:19`,
  also inside the JSON `payload` as `rule_id`), written and queried in
  `crates/wuphf-storage/src/sqlite.rs:160-165, 216-224`. **The ids do not
  change in this plan**, so those rows need nothing. The one stored thing
  that changes shape is a `rules` override in the `settings` table (plan
  036/037), which holds the old JSON list shape: Step 3 migrates it.
- Where the TOML rule format appears (`grep -rln "\[\[rules\]\]" --exclude-dir=target --exclude-dir=.git .`):
  `config/example.toml` (two live rules plus commented examples),
  `docs/index.html` (an even older `[[rules.sides]]` snippet, lines ~240-255),
  comments in `connectors/{whatsapp,telegram,instagram,whatsapp-rust}/config.example.toml`
  and `connectors/{whatsapp,instagram}/README.md`, and the tests in
  `crates/wuphf-settings/src/tests.rs` moved there by 035
  (`example_configuration_parses`, `one_to_one_rule_and_filters_parse`) plus
  037's settings tests that `set("rules", json!([…]))`.
  `plans/025-rust-whatsapp-connector.md` also mentions it; plans are history —
  leave them.
- botconf (pinned tag): `RuntimeSetting::per_entry(key, description, &Kind)`
  declares a map setting whose entries are set one by one as `<key>.<name>`;
  `SettingsStore::set("rules.<id>", value)`, `delete_entry("rules.<id>")`,
  `set("rules", whole_map)` all work on it; entry names must satisfy
  `botconf::keys::is_entry_name` (non-empty, no `.`).
- `crates/wuphf-storage/src/migration_tests.rs` — `database_before(version)`
  builds a database up to a migration; existing cases
  `migration_0011_rewrites_stored_json_to_current_names` and
  `migration_0013_reshapes_stored_dispatches` are the model.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Settings tests | `cargo test -p wuphf-settings` | exit 0 |
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline + new tests |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| Old format gone | `grep -rn "\[\[rules\]\]\|rules\.flow\|rules\.sides" --exclude-dir=target --exclude-dir=.git --exclude-dir=plans .` | no output |

## Scope

**In scope**: `crates/wuphf-settings/src/{config.rs,schema.rs,tests.rs}`;
`migrations/0016_rules_override_as_map.sql` (create);
`crates/wuphf-storage/src/migration_tests.rs`; `config/example.toml`;
`docs/index.html`, `docs/architecture.md`; the connector
`config.example.toml` comments and READMEs listed above.

**Out of scope**: `wuphf_model::Rule` / `RuleKind` / `ChatFilter` (unchanged);
`wuphf-core` routing logic; the `conversations` table; old plan files.

## Git workflow

- Commit on the current branch. Do not create a branch, push, or open a PR
  unless the operator instructed it.
- Commits:
  1. `feat(settings)!: key rules by id`
  2. `feat(storage): migrate a stored rules override to the map shape`
  3. `docs: rules are keyed by id`

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ record it. Also record, on a **copy** of the local dev database,
`sqlite3 wuphf.sqlite.copy "SELECT key FROM settings WHERE key = 'rules' OR key LIKE 'rules.%'"`
(there is no production database; this tells you whether Step 3's migration
has anything to do locally).

### Step 1: The entry type and the map

In `crates/wuphf-settings/src/config.rs`:

```rust
/// One routing rule, as written under `[rules.<id>]`. The id is the key.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleEntry {
    /// Lower numbers are tried first when several rules match the same chat;
    /// ties go by id. Only needed when rules overlap.
    #[serde(default)]
    pub priority: i32,
    #[serde(flatten)]
    pub flow: RuleKind,
}
```

`Configuration.rules` becomes `BTreeMap<String, RuleEntry>` (`#[serde(default)]`).

In `derive`:

- reject an id that is not a valid entry name with
  `rule id {id:?} must be non-empty and must not contain '.'`;
- build `Vec<Rule>` sorted by `(priority, id)`:
  `Rule { id: RuleId(id.clone()), flow: entry.flow.clone() }`;
- pass it to `RoutingConfig::new` as before (then `validate` as in 037).

In `schema.rs`, replace the `rules` setting with:

```rust
RuntimeSetting::per_entry(
    "rules",
    "Routing rules by id: which chats mirror each other. Lower priority numbers match first.",
    &Kind::Json,
)
.titled("Rules"),
```

**Verify**: `cargo build -p wuphf-settings` → exit 0.

### Step 2: Configs and tests in the new shape

`config/example.toml` — rewrite the rules section. The two live rules become:

```toml
[rules.wa-to-telegram-mirror]
kind = "many-to-one"
source_connector = "whatsapp-main"
destination_connector = "telegram-main"
destination_chat = { chat_id = "telegram:channel:2060340733" }

[rules.ig-to-telegram-mirror]
kind = "many-to-one"
source_connector = "instagram-main"
destination_connector = "telegram-main"
destination_chat = { chat_id = "telegram:channel:2060340733" }
```

The commented examples follow the same shape (`# [rules.direct-pair]`,
`# kind = "one-to-one"`, …; the filter becomes
`# [rules.<id>.source_chats_filter]`). Update the explanatory comments: rules
are keyed by id; the id is also how conversations remember their rule
(renaming a rule starts new mirror threads); `priority` and when it matters.

Tests in `crates/wuphf-settings/src/tests.rs`:

- `example_configuration_parses`: assert `configuration.rules.len() == 2` and
  the same per-rule checks, iterating the map.
- `one_to_one_rule_and_filters_parse`: rewrite the TOML to
  `[rules.direct]` / `[rules.filtered-mirror]` with a
  `[rules.filtered-mirror.source_chats_filter]` table; same assertions.
- 037's runtime tests: `set("rules", json!([…]))` becomes
  `set("rules", json!({ "<id>": { "kind": …, … } }))`.

New tests:

- `rules_are_ordered_by_priority_then_id` — three rules `b` (priority 0),
  `a` (priority 0), `z` (priority -1): derived order is `z, a, b`.
- `a_one_to_one_exception_beats_a_catch_all` — a many-to-one catch-all `all`
  and a one-to-one `vip` (priority -1) for one source chat:
  `derived.routing.match_source(…vip chat…)` returns rule `vip`.
- `rule_id_field_is_rejected` — `[rules.x]` with `id = "x"` fails to load
  with a message saying the id is the key. Serde will **not** catch this on
  its own: `#[serde(flatten)]` cannot be combined with
  `deny_unknown_fields`, and `RuleKind`'s variants ignore unknown keys. So
  `RuleEntry` gets a guard field
  `#[serde(default, skip_serializing)] id: Option<serde::de::IgnoredAny>` —
  if it is `Some`, `derive` rejects the rule with
  `rule {key:?}: remove the id field; a rule's id is its key`. (Old configs
  pasted unchanged then fail loudly instead of silently dropping the id.)
- `rule_id_with_a_dot_is_rejected` — `[rules."a.b"]` → `InvalidBase` naming it.
- `a_single_rule_can_be_set_and_removed_at_runtime` —
  `set("rules.extra", json!({…}))` adds a third rule while the file's two
  remain; `delete_entry("rules.extra")` removes it; `unset("rules")` returns
  to the file's two.

**Verify**: `cargo test -p wuphf-settings` → all pass; the "Old format gone"
grep may still list docs and connector files (Step 4 fixes them).

Commit 1.

### Step 3: Migrate a stored `rules` override

A `rules` override stored by plan 037 holds the old list shape
(`[{"id": …, "flow": {…}}, …]`) and would now fail to load (botconf would
ignore it with a warning, silently dropping the owner's rules). Per the
owner's rule — stored-shape changes get a SQL data migration, never serde
aliases — add `migrations/0016_rules_override_as_map.sql`:

```sql
-- Rules moved from a list to a map keyed by rule id (plan 038). Rewrite a
-- stored `rules` override from the list shape [{"id": …, "flow": {…}}] to
-- {"<id>": {…flow…, "priority": <list position>}}. The list position becomes
-- the priority so the first-match order is preserved exactly.
UPDATE settings
SET value = (
    SELECT json_group_object(
        json_extract(rule.value, '$.id'),
        json_set(json_extract(rule.value, '$.flow'), '$.priority', rule.key)
    )
    FROM json_each(settings.value) AS rule
)
WHERE key = 'rules' AND json_type(value) = 'array';
```

(An empty list becomes `{}`. No `rules.<id>` rows can exist yet: before this
plan `rules` was not per-entry.)

Add `migration_0016_reshapes_a_stored_rules_override` to
`crates/wuphf-storage/src/migration_tests.rs`: `database_before(16)`, insert a
`settings` row `rules` holding a two-rule list in the old shape (one
one-to-one, one many-to-one with a `not` filter) and an unrelated row
`notify` holding `[]`; run the remaining migrations; assert the `rules` value
is the expected object (compare parsed JSON, not text — `json_group_object`'s
key order and spacing are SQLite's) with priorities `0` and `1`, and that
`notify` is byte-identical. Also assert that the migrated value deserializes
into `BTreeMap<String, RuleEntry>`-shaped JSON — `wuphf-storage` cannot see
`RuleEntry`, so check it structurally: each entry has `kind` and
`priority` and no `id`/`flow`.

**Verify**: `cargo test -p wuphf-storage` → all pass. Then, if Step 0 found
a `rules` row in the copy of the dev database, run the server once against
another fresh copy and confirm the log has no "ignoring an invalid runtime
setting" warning for `rules`.

Commit 2.

### Step 4: Docs and connector comments

- `docs/index.html` "Routing rules" section: replace the stale
  `[[rules]]`/`[[rules.sides]]` snippet with the `[rules.<id>]` example from
  `config/example.toml`.
- `docs/architecture.md`: in the runtime settings paragraph (from 037), add
  that each rule is its own entry (`rules.<id>`), and the priority rule.
- Connector `config.example.toml` comments and the two READMEs: replace
  "the core's `[[rules]]`" with "the core's `[rules.<id>]` entries".

**Verify**: the "Old format gone" grep → no output.

Commit 3.

### Step 5: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0.

## Test plan

- `wuphf-settings`: updated `example_configuration_parses`,
  `one_to_one_rule_and_filters_parse`, 037's runtime tests in the map shape,
  plus `rules_are_ordered_by_priority_then_id`,
  `a_one_to_one_exception_beats_a_catch_all`, `rule_id_field_is_rejected`,
  `rule_id_with_a_dot_is_rejected`,
  `a_single_rule_can_be_set_and_removed_at_runtime`.
- `wuphf-storage`: `migration_0016_reshapes_a_stored_rules_override`.

## Done criteria

- [ ] `cargo test --workspace` exits 0; the tests above exist by name
- [ ] clippy and fmt clean; the "Old format gone" grep prints nothing
- [ ] `wuphf_model::Rule`, `RuleKind`, `ChatFilter` unchanged
      (`git diff 643829b..HEAD -- crates/wuphf-model/src/routing.rs` empty)
- [ ] `git diff --stat 643829b..HEAD` lists only in-scope paths for this plan
- [ ] `plans/README.md` status row updated

## STOP conditions

- `#[serde(flatten)]` of the internally tagged `RuleKind` fails to
  deserialize through figment or to round-trip through botconf (it buffers
  through serde's `Content`; nested internally tagged `ChatFilter` should
  work, but if it does not, report the error — do not change `RuleKind`'s
  tagging).
- The migration's JSON functions are unavailable in the bundled SQLite
  (`json_each`, `json_group_object`, `json_set` — all present since SQLite
  3.9/3.38; sqlx bundles a recent one) — report the version.
- Any test outside the files listed in Scope breaks because it parses rules
  from TOML.

## Maintenance notes

- Renaming a rule (changing its key) is a new rule: conversations recorded
  under the old id stop, and new ones start with new mirror threads. The
  settings panel (plan 041) should say so when renaming is offered.
- If priorities turn out to be confusing in practice, an alternative is to
  forbid overlapping rules in `RoutingConfig::validate`; that would make
  `priority` unnecessary but rules out the "exception + catch-all" pattern.
