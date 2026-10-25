# Plan 038: Key routing rules by id (`[rules.<id>]`) and write chats as `connector:chat#thread`

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-settings crates/wuphf-model/src/routing.rs crates/wuphf-core/src/config.rs config/example.toml migrations`
> Plan 037 must have landed (`rules` and `notify` are runtime keys,
> `RoutingConfig::validate` exists); if not, STOP. Find the next free
> migration number with `ls migrations/` (expected `0016`, after plan 036's
> `0015_settings.sql`) and use it wherever this plan says `0016`.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW–MED — changes the config file format for rules and `notify`
  (every config with rules or notice targets must be rewritten); routing
  behaviour and every stored message-database shape are unchanged
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
field.

The settings panel should also show chats by **name** ("Family group"), not
as raw JSON objects. botconf's `Kind::Ref` (botconf plan 003) gives a value a
domain and lets the front end look its name up, but a ref is a **string**.
wuphf writes chats as tables (`{ connector_id, chat_id, thread_id }`). So this
plan, which already rewrites the rule format, also gives a chat on a
connector one canonical string form, `connector:chat#thread`, and uses it
everywhere the config names a chat: `notify` and the chats inside rules. Plan
039 then declares those fields as `Kind::Ref { domain: "wuphf.chat" }` and
supplies the names.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Rules are a map keyed by rule id, in the file (`[rules.<id>]`) and as a per-entry runtime setting (`rules.<id>` can be set or removed on its own; the panel lists each rule as an entry). |
| 2 | Rules live in the config file and the settings table, not in the message database, so the format change itself needs no migration of message data. |
| 3 | Rule order is an optional `priority` (default 0, lower first), then the id. Confirmed by the owner 2026-10-25 over the alternative of forbidding overlapping rules. |
| 4 | Every chat the config names is written as one string, `connector:chat` or `connector:chat#thread`, so the settings panel can treat it as a `wuphf.chat` reference and show its name (owner, 2026-10-25). |

### Decisions this plan makes (confirmed by the owner 2026-10-25)

- **The id comes only from the key.** An entry has no `id` field; writing one
  is an error. The `wuphf_model::Rule { id, flow }` type is unchanged — the
  settings layer builds it from the key — so the core, the storage and every
  test that builds `Rule` values stay as they are.
- **No nested `flow` table.** `kind = "many-to-one"` and the rule's fields sit
  directly under `[rules.<id>]`. One level less to type, and a per-entry
  value from the panel is a single flat object.
- **Order: an optional `priority` (default 0), then the id.** Rule order
  matters today: `RoutingConfig::match_source` returns the *first* matching
  rule end, so a one-to-one exception listed before a many-to-one catch-all
  wins. A map has no order (figment's maps are sorted by key, and so is a
  JSON object stored by botconf), so order must be explicit. Lower priority
  numbers match first; ties go by id. Configs without overlapping rules never
  need to set it.
- **Rule ids must be valid entry names**: non-empty and without `.` (botconf
  addresses an entry as `rules.<id>`). Checked in `derive`.

### The chat string form (decided while planning; confirmed with decision 4)

`connector:chat` or `connector:chat#thread`, for example
`telegram-main:telegram:channel:2060340733#12` or
`whatsapp-main:12345@s.whatsapp.net`.

- **The connector ends at the first `:`.** Connector ids are chosen in this
  config file, so wuphf can forbid `:` in them; chat ids are chosen by the
  platform and do contain `:` (the Telegram connector's ids are
  `telegram:user:<n>`, `telegram:chat:<n>` and `telegram:channel:<n>`, see
  `connectors/telegram/internal/telegram/targets.go:117-121`; a future Matrix
  connector's room ids are `!room:server`). Splitting at the first `:` keeps
  every such chat id intact without escaping it. `:` also reads naturally
  (`tg-main:…`, the form the owner saw when choosing this).
- **The thread follows the last `#`.** No platform wuphf bridges today puts a
  `#` in a chat id (Telegram ids above; WhatsApp JIDs and LIDs are
  `<digits>@s.whatsapp.net`, `<digits>@lid`, `<id>@g.us`; Instagram thread
  ids are decimal, `connectors/instagram/internal/instagram/updates.go:108`),
  but Matrix room aliases start with one (`#room:server`). So inside the chat
  and thread parts, `%` is written `%25` and `#` is written `%23`. `Display`
  always escapes those two characters; `FromStr` decodes exactly `%23` and
  `%25` and rejects any other `%` sequence, so every value has one spelling
  and round-trips.
- **Connector ids are restricted**: non-empty, and none of `:`, `#`, `%` or
  whitespace. Every id in use today (`whatsapp-main`, `whatsapp-rust`,
  `instagram-main`, `telegram-main`) passes. The check goes into `derive`
  next to 037's `RoutingConfig::validate`.
- **Empty parts are errors**: an empty chat (`telegram-main:`), an empty
  thread (`…#`), or no `:` at all.
- **Only the config changes shape; stored JSON does not.** `ChatRef` and
  `ChatAddress` are persisted inside SQLite JSON: `Conversation.source` in
  `conversations.payload` (`crates/wuphf-model/src/routing.rs:322-336`,
  written by `migrations/0014_merge_conversations.sql:36-41`) and
  `ChatAddress` in dispatch payloads (`crates/wuphf-model/src/dispatch.rs:75`).
  Their serde derives therefore stay exactly as they are. The string form is
  (1) `Display` and `FromStr` on `ChatRef` in `wuphf-model`, with no serde
  change, and (2) a **config-facing newtype** in `wuphf-settings`,
  `ChatTarget(ChatRef)`, whose serde goes through that string. A newtype
  rather than `#[serde(with = …)]` because the fields are a `Vec` (`notify`,
  filter lists) as well as single values, and one type with its own impls
  covers every position without a `with` helper per container shape. It also
  keeps the model crate free of config concerns.
- **Rule chats name their connector.** The config-facing rule shape replaces
  each `*_connector` + `*_chat` pair with one chat string, so every chat a
  rule names is a full reference the panel can look up:

  | Model field (`RuleKind`, unchanged) | Config key under `[rules.<id>]` |
  |---|---|
  | one-to-one `source_connector` + `source_chat` | `source = "<connector>:<chat>[#thread]"` |
  | one-to-one `destination_connector` + `destination_chat` | `destination = "…"` |
  | many-to-one `source_connector` | `source_connector = "<connector>"` (a connector, not a chat) |
  | many-to-one `source_chats_filter` (`Only` / `Not` / `All`) | `only_chats = ["<connector>:<chat>", …]` or `except_chats = […]`; neither means every chat |
  | many-to-one `destination_connector` + `destination_chat` | `destination = "…"` |

  The filter is flattened into two optional lists instead of a nested
  `{ kind, chats }` table, so a rule is one flat object whose fields the
  settings panel can edit one by one (plan 039 makes the rule entry a botconf
  form; `only_chats` and `except_chats` form a group, so setting one clears
  the other). Setting both is an error
  (`rule {id:?}: use only_chats or except_chats, not both`). A filter chat
  must be on the rule's `source_connector`; `derive` rejects
  any other (`rule {id:?}: filter chat {chat} is not on source connector
  {connector}`), since the model's filter holds bare addresses on that
  connector. `notify` becomes a list of chat strings.

## Current state

- `crates/wuphf-model/src/routing.rs` — the model, all unchanged by this plan
  except for new `Display`/`FromStr` impls:

  ```rust
  // :45-51
  pub struct ChatAddress { pub chat_id: String, #[serde(default)] pub thread_id: Option<String> }
  // :76-82, flattened when (de)serialized
  pub struct ChatRef { pub connector_id: ConnectorId, #[serde(flatten)] pub address: ChatAddress }
  // :157-187
  pub struct Rule { pub id: RuleId, pub flow: RuleKind }

  #[serde(rename_all = "kebab-case", tag = "kind")]
  pub enum RuleKind {
      OneToOne { source_connector, source_chat, destination_connector, destination_chat },
      ManyToOne { source_connector, #[serde(default)] source_chats_filter: ChatFilter,
                  destination_connector, destination_chat },
  }
  ```

  `ChatFilter` is internally tagged (`kind`, lowercase) with
  `Only { chats: Vec<ChatAddress> }` / `Not { chats: Vec<ChatAddress> }`.
  `RuleId(pub String)` is in `crates/wuphf-model/src/ids.rs`.
- `crates/wuphf-core/src/config.rs` — `RoutingConfig::match_source`
  iterates `self.rules` in order and returns the first accepting end; this is
  why order matters. `notice_targets: Vec<ChatRef>`.
- `crates/wuphf-settings/src/config.rs` (from 035) — `Configuration.rules:
  Vec<Rule>` and `Configuration.notify: Vec<ChatRef>` (both
  `#[serde(default)]`); `derive` passes them to `RoutingConfig::new` /
  `with_notice_targets`. `schema.rs` (from 037) declares
  `RuntimeSetting::new("rules", …).kind(Kind::Json)` and the same for
  `notify`.
- Where `ChatRef`/`ChatAddress` JSON is stored (must not change):
  `conversations.payload` `$.source` (0014), `dispatches.payload` target
  addresses (0013), the provisioning stash's `origin_endpoint`. The
  regression net is `crates/wuphf-storage/src/migration_tests.rs`, whose
  cases assert payloads are byte-identical to what the model writes.
- Rule ids in SQLite: `conversations.rule_id` (`migrations/0014_merge_conversations.sql:19`,
  also inside the JSON `payload` as `rule_id`), written and queried in
  `crates/wuphf-storage/src/sqlite.rs:160-165, 216-224`. **The ids do not
  change in this plan**, so those rows need nothing. The stored things that
  change shape are the `rules` and `notify` overrides in the `settings` table
  (plans 036/037), which hold the old JSON shapes: Step 4 migrates them.
- Where the TOML formats appear
  (`grep -rln "\[\[rules\]\]\|\[\[notify\]\]" --exclude-dir=target --exclude-dir=.git --exclude-dir=plans .`):
  `config/example.toml` (one `[[notify]]`, two live rules plus commented
  examples), `docs/index.html` (an even older `[[rules.sides]]` snippet,
  lines ~240-255), `docs/architecture.md` (`[[notify]]`), comments in
  `connectors/{whatsapp,telegram,instagram,whatsapp-rust}/config.example.toml`
  and `connectors/{whatsapp,instagram}/README.md`, and the tests in
  `crates/wuphf-settings/src/tests.rs` moved there by 035
  (`example_configuration_parses`, `one_to_one_rule_and_filters_parse`) plus
  037's settings tests that `set("rules", json!([…]))` and
  `set("notify", json!([…]))`. `plans/025-rust-whatsapp-connector.md` also
  mentions the old shapes; plans are history — leave them.
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
| Model tests | `cargo test -p wuphf-model` | exit 0 |
| Settings tests | `cargo test -p wuphf-settings` | exit 0 |
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline + new tests |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| Old formats gone | `grep -rn "\[\[rules\]\]\|\[\[notify\]\]\|rules\.flow\|rules\.sides" --exclude-dir=target --exclude-dir=.git --exclude-dir=plans .` | no output |

## Scope

**In scope**: `crates/wuphf-model/src/routing.rs` (new `Display`/`FromStr`
impls and their tests only); `crates/wuphf-settings/src/{config.rs,schema.rs,tests.rs}`;
`migrations/0016_rules_and_notify_overrides.sql` (create);
`crates/wuphf-storage/src/migration_tests.rs`; `config/example.toml`;
`docs/index.html`, `docs/architecture.md`; the connector
`config.example.toml` comments and READMEs listed above.

**Out of scope**: the serde attributes of `ChatRef`, `ChatAddress`, `Rule`,
`RuleKind` and `ChatFilter` (unchanged); `wuphf-core` routing logic; the
`conversations` table; old plan files.

## Git workflow

- Commit on the current branch. Do not create a branch, push, or open a PR
  unless the operator instructed it.
- Commits:
  1. `feat(model): a string form for chat references`
  2. `feat(settings)!: key rules by id and write chats as strings`
  3. `feat(storage): migrate stored rules and notify overrides`
  4. `docs: rules are keyed by id; chats are connector:chat`

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ record it. Also record, on a **copy** of the local dev database,
`sqlite3 wuphf.sqlite.copy "SELECT key FROM settings WHERE key IN ('rules', 'notify') OR key LIKE 'rules.%'"`
(there is no production database; this tells you whether Step 4's migration
has anything to do locally).

### Step 1: The string form in the model

In `crates/wuphf-model/src/routing.rs`, add below `ChatRef`:

```rust
/// The text form of a chat reference, `connector:chat` or
/// `connector:chat#thread`, as the config file and the settings panel write
/// it. The connector ends at the first `:` (connector ids may not contain
/// one; chat ids often do). Inside the chat and thread parts `%` is written
/// `%25` and `#` is written `%23`, so the thread separator is unambiguous.
/// Stored JSON does not use this form: `ChatRef` still serializes as fields.
impl fmt::Display for ChatRef { … }

impl FromStr for ChatRef {
    type Err = ChatRefParseError;
    …
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatRefParseError {
    #[error("{0:?} has no connector: write it as connector:chat")]
    MissingConnector(String),
    #[error("{0:?} has an empty chat id")]
    EmptyChat(String),
    #[error("{0:?} has an empty thread id after '#'")]
    EmptyThread(String),
    #[error("{0:?} has an invalid escape; only %23 (#) and %25 (%) are allowed")]
    InvalidEscape(String),
}
```

Parsing: split at the first `:` (`MissingConnector` when there is none or the
connector part is empty); in the rest, split at the **last** `#` if any; then
unescape each part. Add `pub fn is_valid_connector_id(id: &str) -> bool`
(non-empty; no `:`, `#`, `%` or whitespace) next to it for 035/037's
validation to call. If `thiserror` is not yet a `wuphf-model` dependency, add
it from `[workspace.dependencies]`.

Tests in the same file's test module (create one if absent):

- `chat_refs_round_trip` — for each of `telegram-main:telegram:channel:2060340733`,
  `telegram-main:telegram:channel:2060340733#12`,
  `whatsapp-main:12345@s.whatsapp.net`, `whatsapp-main:98765@lid`,
  `whatsapp-main:120363@g.us`, `instagram-main:340282366841710300949128`,
  `matrix-main:!room:example.org`, a chat id `#alias:example.org`
  (written `matrix-main:%23alias:example.org`) and a chat id containing `%`:
  `parse` then `to_string` returns the input, and `parse` gives the expected
  `connector_id`, `chat_id` and `thread_id`.
- `chat_ref_parse_errors` — `"no-colon"`, `":chat"`, `"conn:"`,
  `"conn:chat#"`, `"conn:a%2Fb"` give the matching error variants.
- `chat_ref_json_shape_is_unchanged` — `serde_json::to_value` of a `ChatRef`
  with a thread is still `{"connector_id": …, "chat_id": …, "thread_id": …}`.

**Verify**: `cargo test -p wuphf-model` → all pass.

Commit 1.

### Step 2: Config-facing types

In `crates/wuphf-settings/src/config.rs`:

```rust
/// A chat in the config file and the settings store, written
/// `connector:chat#thread` (see `ChatRef`'s `Display`). Only config uses this
/// form; stored message data keeps `ChatRef`'s field shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTarget(pub ChatRef);
// Serialize: serializer.collect_str(&self.0)
// Deserialize: String, then ChatRef::from_str, mapping the error with
// serde::de::Error::custom (the message names the bad value).

/// One routing rule, as written under `[rules.<id>]`. The id is the key.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleEntry {
    /// Lower numbers are tried first when several rules match the same chat;
    /// ties go by id. Only needed when rules overlap.
    #[serde(default)]
    pub priority: i32,
    #[serde(flatten)]
    pub flow: RuleEntryKind,
    /// Present only when an old `[[rules]]` entry was pasted unchanged; see
    /// `derive`.
    #[serde(default, skip_serializing)]
    id: Option<serde::de::IgnoredAny>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum RuleEntryKind {
    OneToOne { source: ChatTarget, destination: ChatTarget },
    ManyToOne {
        source_connector: ConnectorId,
        destination: ChatTarget,
        /// Only these source chats are mirrored. Excludes `except_chats`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only_chats: Option<Vec<ChatTarget>>,
        /// Every source chat except these is mirrored. Excludes `only_chats`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        except_chats: Option<Vec<ChatTarget>>,
    },
}
```

`RuleEntryKind` mirrors `RuleKind` with chat strings; add
`fn into_rule_kind(self, rule_id: &str) -> Result<RuleKind, String>` that
splits each `ChatTarget` back into the model's connector + address fields,
turns `only_chats` / `except_chats` into `ChatFilter::Only` / `ChatFilter::Not`
(neither: `ChatFilter::All`; both: an error), and checks that every filter
chat's connector equals `source_connector`.

`Configuration` changes:

- `rules: BTreeMap<String, RuleEntry>` (`#[serde(default)]`);
- `notify: Vec<ChatTarget>` (`#[serde(default)]`).

In `derive`:

- reject a connector id that fails `is_valid_connector_id`
  (`connector id {id:?} must not be empty or contain ':', '#', '%' or spaces`);
- reject a rule id that is not a valid entry name with
  `rule id {id:?} must be non-empty and must not contain '.'`;
- reject a rule whose guard `id` field is `Some` with
  `rule {key:?}: remove the id field; a rule's id is its key`;
- build `Vec<Rule>` sorted by `(priority, id)`:
  `Rule { id: RuleId(id.clone()), flow: entry.flow.clone().into_rule_kind(&id)? }`;
- `notify` becomes `Vec<ChatRef>` by unwrapping each `ChatTarget`;
- pass both to `RoutingConfig` as before (then `validate` as in 037).

In `schema.rs`, replace the `rules` setting with:

```rust
RuntimeSetting::per_entry(
    "rules",
    "Routing rules by id: which chats mirror each other. Lower priority numbers match first.",
    &Kind::Json,
)
.titled("Rules"),
```

`notify` stays `Kind::Json` here; plan 039 turns both into
`wuphf.chat` refs once botconf's `Kind::Ref` is available.

**Verify**: `cargo build -p wuphf-settings` → exit 0.

### Step 3: Configs and tests in the new shape

`config/example.toml`:

```toml
notify = ["telegram-main:telegram:user:704338780"]
```

(placed before the first table, with the existing explanation kept above it),
and the two live rules:

```toml
[rules.wa-to-telegram-mirror]
kind = "many-to-one"
source_connector = "whatsapp-main"
destination = "telegram-main:telegram:channel:2060340733"

[rules.ig-to-telegram-mirror]
kind = "many-to-one"
source_connector = "instagram-main"
destination = "telegram-main:telegram:channel:2060340733"
```

The commented examples follow the same shape:

```toml
# [rules.direct-pair]
# kind = "one-to-one"
# source = "whatsapp-main:12345@s.whatsapp.net"
# destination = "telegram-main:-100123#12"
#
# A many-to-one rule may limit its source chats with `only_chats = [...]`
# (an allowlist) or `except_chats = [...]` (a blocklist), not both:
# except_chats = ["whatsapp-main:12345@s.whatsapp.net"]
```

Update the explanatory comments: a chat is `connector:chat`, optionally
`#thread`, where the connector is everything before the first `:`; `#` and
`%` inside ids are written `%23` and `%25`; rules are keyed by id; the id is
also how conversations remember their rule (renaming a rule starts new
mirror threads); `priority` and when it matters. Keep the paragraph about
WhatsApp's phone JID and LID forms. **`notify` is a top-level key, so it must
sit above the first `[table]`** (TOML would otherwise nest it).

Tests in `crates/wuphf-settings/src/tests.rs`:

- `example_configuration_parses`: assert `configuration.rules.len() == 2`,
  the same per-rule checks iterating the map, and one notice target with
  `connector_id == "telegram-main"`.
- `one_to_one_rule_and_filters_parse`: rewrite the TOML to `[rules.direct]`
  (one-to-one with a `#thread` destination) / `[rules.filtered-mirror]` with
  `except_chats = [...]`, and a third rule with `only_chats`; assert the derived
  `RuleKind`s equal hand-built model values (connector and address split
  correctly, thread kept).
- 037's runtime tests: `set("rules", json!([…]))` becomes
  `set("rules", json!({ "<id>": { "kind": …, "source": "…", … } }))`, and
  `set("notify", json!([{…}]))` becomes `set("notify", json!(["conn:chat"]))`.

New tests:

- `rules_are_ordered_by_priority_then_id` — three rules `b` (priority 0),
  `a` (priority 0), `z` (priority -1): derived order is `z, a, b`.
- `a_one_to_one_exception_beats_a_catch_all` — a many-to-one catch-all `all`
  and a one-to-one `vip` (priority -1) for one source chat:
  `derived.routing.match_source(…vip chat…)` returns rule `vip`.
- `rule_id_field_is_rejected` — `[rules.x]` with `id = "x"` fails to load
  with the "remove the id field" message. Serde will **not** catch this on
  its own (`#[serde(flatten)]` cannot be combined with
  `deny_unknown_fields`), which is why the guard field exists.
- `rule_id_with_a_dot_is_rejected` — `[rules."a.b"]` → `InvalidBase` naming it.
- `filter_chat_on_another_connector_is_rejected` — a many-to-one rule on
  `whatsapp-main` whose `except_chats` lists `instagram-main:1` fails with
  the "not on source connector" message.
- `only_and_except_chats_together_are_rejected` — a rule with both lists
  fails with the "not both" message.
- `invalid_chat_strings_are_rejected` — `notify = ["no-colon"]` fails to load
  and the error names the value.
- `connector_ids_with_separators_are_rejected` — a `[[connectors]]` id
  `"tg:main"` fails.
- `a_single_rule_can_be_set_and_removed_at_runtime` —
  `set("rules.extra", json!({…}))` adds a third rule while the file's two
  remain; `delete_entry("rules.extra")` removes it; `unset("rules")` returns
  to the file's two.

**Verify**: `cargo test -p wuphf-settings` → all pass; the "Old formats gone"
grep may still list docs and connector files (Step 5 fixes them).

Commit 2.

### Step 4: Migrate stored `rules` and `notify` overrides

Overrides stored by plan 037 hold the old shapes — `rules` as
`[{"id": …, "flow": {…}}, …]` with `source_connector`/`source_chat` objects,
`notify` as `[{"connector_id": …, "chat_id": …, "thread_id": …}]` — and would
now fail to load (botconf would ignore them with a warning, silently dropping
the owner's settings). Per the owner's rule — stored-shape changes get a SQL
data migration, never serde aliases — add
`migrations/0016_rules_and_notify_overrides.sql`. It needs one helper
expression, written inline wherever a chat object becomes a string (SQLite
has no user functions in migrations):

```sql
-- chat string for connector C and address object A:
--   C || ':' || esc(A.chat_id) || coalesce('#' || esc(A.thread_id), '')
-- where esc(x) = replace(replace(x, '%', '%25'), '#', '%23')
```

Statements:

1. `notify`: rewrite a stored list of objects into a list of strings:

   ```sql
   UPDATE settings
   SET value = (
       SELECT json_group_array(
           json_extract(t.value, '$.connector_id') || ':' ||
           replace(replace(json_extract(t.value, '$.chat_id'), '%', '%25'), '#', '%23') ||
           coalesce('#' || replace(replace(json_extract(t.value, '$.thread_id'), '%', '%25'), '#', '%23'), '')
       )
       FROM json_each(settings.value) AS t
   )
   WHERE key = 'notify' AND json_type(value) = 'array';
   ```

2. `rules`: rewrite the list into a map keyed by id, with the list position
   as `priority` so the first-match order is preserved exactly, and each
   flow reshaped:
   - one-to-one: `{kind, source: <string from source_connector + source_chat>,
     destination: <string from destination_connector + destination_chat>,
     priority}`;
   - many-to-one: `{kind, source_connector, destination: <string>,
     priority}`, plus `only_chats` (from a filter of kind `only`) or
     `except_chats` (from `not`) holding the filter's `chats` as strings,
     using `source_connector` as the connector; a filter of kind `all`, or
     no filter, adds neither key.

   Write it as one `UPDATE … SET value = (SELECT json_group_object(id,
   <entry>) FROM json_each(settings.value) …)` with a `CASE` on
   `json_extract(rule.value, '$.flow.kind')`. Keep it readable: one `CASE`
   arm per kind, the chat expression repeated (comment it once at the top).
   An empty list becomes `{}`. No `rules.<id>` rows can exist yet: before
   this plan `rules` was not per-entry.

Add `migration_0016_reshapes_stored_rules_and_notify` to
`crates/wuphf-storage/src/migration_tests.rs`: `database_before(16)`, insert
`settings` rows `rules` (a two-rule list in the old shape: one one-to-one
with a destination thread, one many-to-one with a `not` filter whose chat id
contains `#`) and `notify` (one object with a thread, one without), plus an
unrelated row `timezone`; run the remaining migrations; assert the `rules`
and `notify` values equal the expected JSON (compare parsed JSON, not text —
`json_group_object`'s key order and spacing are SQLite's), with priorities
`0` and `1` and the `#` in the filter chat written `%23`; and that the
unrelated row is byte-identical. `wuphf-storage` cannot see the settings
types, so also assert structurally: every rule entry has `kind` and
`priority` and no `id`/`flow`/`*_chat` keys.

**Verify**: `cargo test -p wuphf-storage` → all pass. Then, if Step 0 found
a `rules` or `notify` row in the copy of the dev database, run the server
once against another fresh copy and confirm the log has no "ignoring an
invalid runtime setting" warning for either.

Commit 3.

### Step 5: Docs and connector comments

- `docs/index.html` "Routing rules" section: replace the stale
  `[[rules]]`/`[[rules.sides]]` snippet with the `[rules.<id>]` example from
  `config/example.toml`, and show `notify = […]`.
- `docs/architecture.md`: replace the `[[notify]]` mention with `notify`; in
  the runtime settings paragraph (from 037), add that each rule is its own
  entry (`rules.<id>`), the priority rule, and the `connector:chat#thread`
  form with its escaping.
- Connector `config.example.toml` comments and the two READMEs: replace
  "the core's `[[rules]]`" with "the core's `[rules.<id>]` entries" and any
  `[[notify]]` with `notify`.

**Verify**: the "Old formats gone" grep → no output.

Commit 4.

### Step 6: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0.

## Test plan

- `wuphf-model`: `chat_refs_round_trip`, `chat_ref_parse_errors`,
  `chat_ref_json_shape_is_unchanged`.
- `wuphf-settings`: updated `example_configuration_parses`,
  `one_to_one_rule_and_filters_parse`, 037's runtime tests in the new shapes,
  plus `rules_are_ordered_by_priority_then_id`,
  `a_one_to_one_exception_beats_a_catch_all`, `rule_id_field_is_rejected`,
  `rule_id_with_a_dot_is_rejected`,
  `filter_chat_on_another_connector_is_rejected`,
  `only_and_except_chats_together_are_rejected`,
  `invalid_chat_strings_are_rejected`,
  `connector_ids_with_separators_are_rejected`,
  `a_single_rule_can_be_set_and_removed_at_runtime`.
- `wuphf-storage`: `migration_0016_reshapes_stored_rules_and_notify`; every
  existing migration test still passes unchanged (proof that stored message
  JSON did not change).

## Done criteria

- [ ] `cargo test --workspace` exits 0; the tests above exist by name
- [ ] clippy and fmt clean; the "Old formats gone" grep prints nothing
- [ ] The serde attributes of `ChatRef`, `ChatAddress`, `Rule`, `RuleKind`
      and `ChatFilter` are unchanged
      (`git diff 643829b..HEAD -- crates/wuphf-model/src/routing.rs` adds
      only the `Display`/`FromStr` impls, the error type, the connector-id
      check and tests)
- [ ] `git diff --stat 643829b..HEAD` lists only in-scope paths for this plan
- [ ] `plans/README.md` status row updated

## STOP conditions

- `#[serde(flatten)]` of the internally tagged `RuleEntryKind` fails to
  deserialize through figment or to round-trip through botconf (it buffers
  through serde's `Content`) — report the error; do not drop the tagging.
- Any **stored** message-database JSON would change shape (an existing
  `migration_tests.rs` case fails, or a `ChatRef`/`ChatAddress` serde
  attribute has to change) — STOP and ask; this plan must not migrate
  message data.
- The migration's JSON functions are unavailable in the bundled SQLite
  (`json_each`, `json_group_object`, `json_group_array`, `json_set` — all
  present since SQLite 3.9/3.38; sqlx bundles a recent one) — report the
  version.
- A connector id in a real config contains `:`, `#`, `%` or whitespace
  (check the local `config/*.toml` and `scripts/` configs) — report it
  rather than loosening the rule.
- Any test outside the files listed in Scope breaks because it parses rules
  or `notify` from TOML.

## Maintenance notes

- Renaming a rule (changing its key) is a new rule: conversations recorded
  under the old id stop, and new ones start with new mirror threads. The
  settings panel (plan 041) should say so when renaming is offered.
- If priorities turn out to be confusing in practice, an alternative is to
  forbid overlapping rules in `RoutingConfig::validate`; that would make
  `priority` unnecessary but rules out the "exception + catch-all" pattern.
- New platforms: if a platform's chat ids could start with something that
  looks like a connector id followed by `:`, nothing breaks — only the
  *first* `:` splits, and connector ids come from the config, not the
  platform.
