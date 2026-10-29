# Plan 044: Reload re-reads the config file, and new rules go through drafts

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Build limits (the owner's laptop)**: prefix every cargo command with
> `nice -n 19` and set `CARGO_BUILD_JOBS=2`. Check and test only the crates you
> touch (`-p`), and run `cargo test --workspace` and clippy once, at the end.
> No release builds, no whatsapp-rust or Go builds (nothing Go changes here),
> no `nix flake check`, no dev-bin installs. Only one agent builds at a time.
>
> **Drift check (run first)**: `git diff --stat da29ba9..HEAD -- crates/wuphf-settings crates/wuphf-cli Cargo.toml README.md docs/architecture.md`
> Expected: empty, or only 041/043 work (which does not touch these). What
> must still hold: the workspace pins botconf, botconf-grpc and botconf-panel
> at `tag = "v0.5.0"` (`Cargo.toml:28-30`); `wuphf_settings::{base, load}` in
> `crates/wuphf-settings/src/load.rs`; `settings::open` in
> `crates/wuphf-cli/src/settings.rs:39` builds the store with
> `wuphf_settings::load(schema, base, …)`; `RULE_FORM`/`RULE_FIELDS` in
> `crates/wuphf-settings/src/schema.rs:146-195`. Otherwise STOP.
>
> **Prerequisite**: botconf plans 010 and 011 landed and pushed, so
> `git ls-remote --tags https://github.com/propheci/botconf` lists `v0.8.0` and
> `v0.9.0`. If `v0.9.0` is missing, STOP.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW-MED — changes what a reload applies (the file is re-read) and
  how the panel edits rules; validation is unchanged
- **Depends on**: 039, 040, 042; botconf 010 (`v0.8.0`) and 011 (`v0.9.0`)
- **Category**: feature / bug
- **Planned at**: commit `da29ba9`, 2026-10-29

## Why this matters

Plan 039 found two gaps in botconf, and the owner decided on both on
2026-10-29:

1. **A reload never saw an edited config file.** botconf kept the figment it
   was loaded with, and figment reads its sources when they are merged. So
   `wuphf settings reload` and the service's `Reload` re-read only the stored
   overrides, and 042 had to word its help around that ("file edits still
   need a restart"). Owner: reload must re-read the file. botconf 010 adds a
   `BaseSource` the store re-reads on every reload; wuphf must pass one.
2. **A new rule could not be created from the panel.** Adding a rule stored
   the form's initial value `{"kind": "many-to-one"}` at once, which the
   schema refuses (no `source_connector`, no `destination`); switching a
   rule's `kind` failed the same way. Owner: add drafts. botconf 011 lets the
   panel build such a value in a draft and store it on Save; wuphf must say
   which rule fields are required, and when.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | A reload re-reads the TOML file and the `WUPHF_CORE_` environment, then the stored overrides. An edit that makes the configuration invalid is refused with the reason; the running configuration stays. |
| 2 | Rules are built in a draft when they need several fields at once: a new rule, and a rule whose `kind` changes. Nothing is stored until Save, and Save is validated by `WuphfSchema` like any other change. |

### Decisions this plan makes (flag to the owner)

- **Keys read only at startup stay startup-only**, and the docs say so:
  `listen`, `database_url`, the connector list (`[[connectors]]`: the hub
  gets it once), the connector token variables (wrapped into
  `ConnectorTokens` by `serve()`, `main.rs:102`), and `functions`. A reload
  updates them in the snapshot (and validates them), but `serve()` does not
  re-read them. Re-reading those is out of scope.
- **`owner_ids` follow a reload.** `WuphfAuthorizer` reads
  `store.current().derived.owners` on every call
  (`crates/wuphf-runtime/src/settings_auth.rs:129-134`), so an `owner_ids` edit
  in the file applies on reload. This is the test 039 could not write
  (`owner_ids_follow_reload`).
- **Rule fields:** `kind` and `destination` are required; `source` is
  required `when kind = one-to-one`; `source_connector` is required `when
  kind = many-to-one`; `only_chats` and `except_chats` apply `when kind =
  many-to-one` (not required); `priority` stays optional. The form's initial
  value stays `{"kind": "many-to-one"}`.

## Current state

- `Cargo.toml:28-30`:
  ```toml
  botconf = { git = "https://github.com/propheci/botconf", tag = "v0.5.0" }
  botconf-grpc = { git = "https://github.com/propheci/botconf", tag = "v0.5.0" }
  botconf-panel = { git = "https://github.com/propheci/botconf", tag = "v0.5.0" }
  ```
- `crates/wuphf-settings/src/load.rs`:
  ```rust
  /// The file, then the environment. Fails if the file is missing.
  pub fn base(path: &Path) -> Result<Figment, LoadError> { … Toml::file_exact(path) … Env::prefixed(ENV_PREFIX).split("__") }

  /// The settings store over `base`, with `storage` holding the overrides.
  pub async fn load(schema: WuphfSchema, base: Figment, storage: impl botconf::Storage)
      -> Result<SettingsStore, botconf::SettingsError> {
      SettingsStore::load(schema, base, storage).await
  }
  ```
  Re-exported from `crates/wuphf-settings/src/lib.rs:23`.
- `crates/wuphf-cli/src/settings.rs:39-58`, `open(path, schema)`: builds
  `base` with `wuphf_settings::base(path)`, extracts `database_url` from it
  (the database holds the overrides, so the URL comes from the file and the
  environment only), connects, then `wuphf_settings::load(schema, base,
  SettingsStorage::new(..))`. `serve()` (`main.rs:91`) and the CLI both call
  `open`.
- `crates/wuphf-cli/src/settings.rs:18-21`, `RELOAD_NOTE` ("a running core
  applies this after \"wuphf settings reload\" or a restart") — still true.
  `settings.rs:26-29`, the `Reload` subcommand's doc: "Ask a running core to
  re-read its stored settings. Edits to the config file still need a
  restart." — becomes false.
- `README.md:92`: `# tell a running core to re-read the stored settings (file
  edits need a restart)`.
- `docs/architecture.md:146-148`: "botconf's reload re-reads the stored
  overrides only; the config file is read when the core starts, so edits to
  it still need a restart."
- `crates/wuphf-settings/src/schema.rs:144-195`, the rule form:
  ```rust
  const RULE: &Kind = &Kind::form(RULE_FORM);
  const RULE_FORM: &Form = &Form::new(RULE_FIELDS, r#"{"kind": "many-to-one"}"#);
  const RULE_FIELDS: &[Field] = &[
      Field::new("kind", "Kind", Kind::one_of(Choices::fixed(RULE_KINDS), false, true)),
      Field::new("priority", "Priority (lower first)", Kind::number(-1000, 1000, "", &[-1, 0, 1], true)),
      Field::new("source", "Source chat (one-to-one)", Kind::reference(domains::CHAT, false)),
      Field::new("source_connector", "Source connector (many-to-one)", Kind::reference(domains::CONNECTOR, false)),
      Field::new("destination", "Destination chat", Kind::reference(domains::CHAT, false)),
      Field::new("only_chats", "Only these chats", Kind::reference(domains::CHAT, true)).group("filter"),
      Field::new("except_chats", "Every chat except these", Kind::reference(domains::CHAT, true)).group("filter"),
  ];
  ```
- `crates/wuphf-settings/src/tests.rs`: `base(...)` is called directly by
  several tests (lines 32, 40, 48, 336, 364, 626, 666); they keep passing a
  `Figment` (botconf 010's `impl BaseSource for Figment`).
- 039's tests for the rule form: `a_rule_is_edited_field_by_field`
  (`crates/wuphf-settings/src/tests.rs:978`) and
  `rule_filters_share_a_form_group` (`tests.rs:1031`). 039's end-to-end
  service tests (the authorizer, `settings_server`) are in
  `crates/wuphf-runtime/src/settings_auth/tests.rs`.

## Commands you will need

```sh
export CARGO_BUILD_JOBS=2
nice -n 19 cargo check -p wuphf-settings -p wuphf-cli --all-targets
nice -n 19 cargo test -p wuphf-settings
nice -n 19 cargo test -p wuphf-cli
nice -n 19 cargo test -p wuphf-runtime settings
nice -n 19 cargo test --workspace          # once, at the end
nice -n 19 cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## Scope

In: `Cargo.toml`, `Cargo.lock`, `crates/wuphf-settings/src/{load.rs,lib.rs,schema.rs,tests.rs}`,
`crates/wuphf-cli/src/{settings.rs,settings/tests.rs}`,
`crates/wuphf-runtime/src/settings_auth/tests.rs` (the reload test),
`README.md`, `docs/architecture.md`, `plans/README.md`.

Out: re-reading startup-only keys in `serve()`; connector settings sections
(040; their bases come from the connectors, and botconf 010 leaves dynamic
sections' bases alone); the Go connectors.

## Git workflow

Commit on the current branch (`plans/runtime-settings`), one commit per step.
Do not push.

## Steps

### Step 0: Baseline

`nice -n 19 cargo test --workspace` → record the count (263 after 040).

### Step 1: Move to botconf 0.9

- `Cargo.toml:28-30`: the three pins to `tag = "v0.9.0"`. `cargo update -p
  botconf -p botconf-grpc -p botconf-panel` (or a plain `cargo check` if the
  update refuses, as 001 found for path-to-git moves).
- Fix whatever 010/011's notes say changed in the API (none expected for
  wuphf: `load` takes `impl BaseSource` and a `Figment` still works; `Field`
  gained builders; `FormField` gained two proto fields, which 040's
  conversion of connector descriptors must accept with their defaults).

**Verify**: `nice -n 19 cargo test -p wuphf-settings -p wuphf-cli` → green,
no behaviour change; the lock moves only the three botconf sources (and
anything 010/011's notes list); tonic stays 0.12.3 and prost 0.13.5.
Commit: `build: move to botconf 0.9`.

### Step 2: A base source that re-reads the file

- `load.rs`: keep `base(path) -> Result<Figment, LoadError>` (tests and the
  startup checks use it). Add
  ```rust
  /// The file and the environment, re-read on every reload.
  pub fn source(path: PathBuf) -> impl botconf::BaseSource + Clone {
      move || base(&path)
  }
  ```
  and change `load` to `base: impl botconf::BaseSource`. Export `source` from
  `lib.rs`.
- `settings::open`: keep the startup `base(path)` for `database_url`, but pass
  `wuphf_settings::source(path.to_path_buf())` to `load`.
- Docs: `settings.rs:28` becomes "Ask a running core to re-read the config
  file, the environment and the stored settings. Reads the token from
  WUPHF_OBSERVER_TOKEN."; `README.md:92` becomes "# tell a running core to
  re-read the config file and the stored settings"; `docs/architecture.md:146-148`
  becomes: "A reload re-reads the config file, the environment and the stored
  overrides; an edit that makes the configuration invalid is refused with the
  reason. `listen`, `database_url`, the connector list, the connector tokens
  and `[functions]` are read only when the core starts, so changing them
  still needs a restart." Keep `RELOAD_NOTE` as it is.

Tests (`crates/wuphf-settings/src/tests.rs`, in a `figment::Jail` like the
existing file tests, or a temp dir):

1. `reload_reads_an_edited_file` — load over `source(path)`, rewrite the
   file's `notify`, `reload()` → the new `notify`.
2. `reload_refuses_an_invalid_file` — rewrite the file with a rule naming an
   unknown connector → `reload()` errs with the reason; `current()` unchanged.

And in the runtime's settings tests (where 039's end-to-end tests start
`settings_server`): `owner_ids_follow_reload` — a connector token with an
actor not in `owner_ids` is refused; add the actor to the file's `owner_ids`,
call the service's `Reload` as the observer → the same request is allowed.

**Verify**: `nice -n 19 cargo test -p wuphf-settings` and
`nice -n 19 cargo test -p wuphf-runtime settings` → green. Mutation: pass the
startup `Figment` to `load` again → tests 1 and `owner_ids_follow_reload`
fail. Commit: `feat(settings): reload re-reads the config file`.

### Step 3: Rules are built in drafts

`schema.rs`, `RULE_FIELDS`, per the design decisions:

```rust
Field::new("kind", …).required(),
Field::new("priority", …),
Field::new("source", …).when("kind", "one-to-one").required(),
Field::new("source_connector", …).when("kind", "many-to-one").required(),
Field::new("destination", …).required(),
Field::new("only_chats", …).group("filter").when("kind", "many-to-one"),
Field::new("except_chats", …).group("filter").when("kind", "many-to-one"),
```

Remove the comment's sentence that a half-filled rule "is refused with the
reason" and say instead that a new rule, or a kind switch, is built in a
draft and saved once complete.

Tests, in `crates/wuphf-settings/src/tests.rs` beside
`a_rule_is_edited_field_by_field`, driving a `botconf_panel::Panel` built
over the same `store(every_token_set(), Path::new(EXAMPLE))` that test uses
(it sets values on the store directly; these tests press panel buttons):

1. `a_rule_is_created_through_a_draft` — add entry `rules.r1` → a draft (the
   store has no `rules.r1`); set `source_connector`, then `destination`
   through the draft's buttons → Save is offered; Save → `rules.r1` stored and
   `LiveRouting` sees it.
2. `switching_a_rules_kind_goes_through_a_draft` — a stored many-to-one rule;
   pick `kind = one-to-one` → a draft, `source_connector` and the filters
   dropped, `source` needed; set `source`; Save → stored as one-to-one.
3. `a_rule_is_edited_field_by_field` stays as it is: the store itself still
   refuses a half-filled rule (`store.set("rules.new", {"kind":
   "many-to-one"})`); only the panel now routes it through a draft. Update
   its doc comment's last sentence to say so.

**Verify**: the touched crates' tests green. Mutation: drop `.required()`
from `destination` → Save is offered too early; test 1 must catch it (assert
Save is absent before `destination` is set). Commit: `feat(settings): build
rules in drafts`.

### Step 4: Gate and index

- `nice -n 19 cargo test --workspace` → Step 0 count + the new tests, green;
  clippy and fmt clean.
- `plans/README.md`: row 044 DONE with a notes bullet (the pins, the tests,
  the mutation checks, deviations).
- Commit: `docs(plans): mark plan 044 done`.

## Test plan

| Test | Crate | Proves |
|---|---|---|
| `reload_reads_an_edited_file` | wuphf-settings | the file is re-read |
| `reload_refuses_an_invalid_file` | wuphf-settings | an invalid edit is refused, the running config stays |
| `owner_ids_follow_reload` | wuphf-runtime | 039's missing test: owners follow the file |
| `a_rule_is_created_through_a_draft` | wuphf-settings | new rules work from the panel |
| `switching_a_rules_kind_goes_through_a_draft` | wuphf-settings | kind switches work |

## Done criteria

- [ ] botconf pinned at `v0.9.0`; the lock moved only botconf sources.
- [ ] The five tests pass; both mutations fail a test.
- [ ] No doc still says file edits need a restart (except for the
      startup-only keys listed).
- [ ] Workspace tests, clippy, fmt clean (niced, 2 jobs).
- [ ] Row 044 DONE with notes.

## STOP conditions

- `v0.9.0` is not on the remote.
- botconf 010 or 011 landed with a different API than their plans (no
  `BaseSource`, no `Field::required`/`when`) and their notes do not say how
  to use it.
- Moving to `v0.9.0` moves tonic or prost, or any non-botconf package in the
  lock beyond what botconf's notes list.
- A connector descriptor (040) fails to convert after the `FormField`
  additions.

## Maintenance notes

- If `serve()` ever re-reads `listen` or the connector list on reload, update
  the startup-only list in `docs/architecture.md`.
- New rule kinds must give their kind-specific fields a `when`, or they show
  for every kind.
