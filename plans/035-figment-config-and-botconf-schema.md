# Plan 035: Load the core configuration through figment and describe it to botconf

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-cli Cargo.toml config/example.toml crates/wuphf-functions/src/config.rs crates/wuphf-core/src/config.rs`
> Plan 034 adds `token_env` to `ConnectorConfiguration` and token wiring to
> `serve()`; if it landed, carry those changes along when moving the structs
> (that alone is not a STOP). Anything else that changed the shape of
> `Configuration` or `serve()`: compare against "Current state" and STOP on a
> mismatch.
>
> **Prerequisite check (run first)**: botconf plan 001 (extract the crates to
> `github.com/propheci/botconf`) must be done and tagged. Run
> `git ls-remote --tags https://github.com/propheci/botconf` → lists a tag
> (expected `v0.1.0`). If the repository or tag does not exist, STOP: this
> plan must not vendor or path-depend on a local copy.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW — no behaviour change: the same TOML keys load into the same
  values; the settings store holds no runtime keys yet
- **Depends on**: botconf plan 001 (tagged and pushed). Independent of 034.
- **Category**: direction (runtime settings, step 1 of wuphf's part)
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

The owner wants wuphf's routing (rules, notice targets, later connector
options) to be changeable at runtime from the platforms themselves, using the
`botconf` framework extracted from the assistant bot into
`github.com/propheci/botconf`. botconf layers stored overrides on top of a
[figment](https://docs.rs/figment) base, validates every change by
deserializing the whole configuration and running a `Schema::derive`, and
hands readers an immutable snapshot.

Before any key can change at runtime, the core configuration has to load the
way botconf expects: as a figment (TOML file, then environment), into a
`Schema::Config` type, with the derived runtime structures (`RoutingConfig`,
the connector list, the resolved functions) built in `derive`. This plan does
exactly that and nothing more. Plans 036 (storage), 037 (live routing) and
038 (rules as a map) build on it.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The core is the single settings store (botconf `SettingsStore`); connectors are front ends to it later (plans 039–041). |
| 2 | botconf is a **git dependency** on `github.com/propheci/botconf`, pinned to a tag, with no features enabled (no sea-orm, no clap). |
| 3 | The base is the TOML file overridden by environment variables (figment `Env`, `__` between levels). |
| 4 | No runtime keys in this plan: `Schema::settings()` returns an empty list. The store uses botconf's `MemoryStorage` until plan 036 swaps in SQLite. |
| 5 | The file must stay valid on its own, and the same keys keep the same meaning: `config/example.toml` loads unchanged. |

### Decisions this plan makes (confirmed by the owner 2026-10-25)

- **Env prefix `WUPHF_CORE_`, not `WUPHF_`.** The core's environment already
  holds `WUPHF_INVOKE_TOKEN`, `WUPHF_PROVIDER_MEDIACONV_TOKEN` and (from plan
  034) `WUPHF_CONNECTOR_*_TOKEN`, and the dev session exports connector
  variables such as `WUPHF_TELEGRAM_APP_ID` and `WUPHF_LOG_LEVEL` into the
  same shell. With a bare `WUPHF_` prefix figment would read every one of
  them as a config key (`invoke_token`, `telegram_app_id`, …). `Configuration`
  does not deny unknown fields, so today that would be silently ignored, but it
  would make denying unknown fields impossible later and invites collisions. So
  `WUPHF_CORE_LISTEN`, `WUPHF_CORE_FUNCTIONS__MAX_DEADLINE_MS`, and so on.
- **A new crate, `wuphf-settings`, hosts the schema** — not `wuphf-cli` and not
  `wuphf-core`. `wuphf-core` must not depend on figment or botconf (the core's
  correctness path should not depend on how configuration is edited; plan 037
  gives it a one-method trait instead). `wuphf-cli` is a binary; plans 036,
  037, 039 and 042 need the schema, the storage adapter and the routing source
  from more than one place (the server, the `settings` CLI command, tests),
  which a library crate serves cleanly.
- **`observer_token` becomes a redacting `Secret`.** botconf's `Snapshot::value`
  serializes the configuration to answer reads. The raw token must never come
  back from that, so `Secret` serializes as `"<redacted>"` (deserialization is
  unchanged, so the TOML key and value are unchanged). The type only matters
  for top-level reads: botconf never serializes the top-level config to rebuild
  it.

## Current state

- `crates/wuphf-cli/src/main.rs:55-99` — the configuration types, private to
  the binary:

  ```rust
  #[derive(Deserialize)]
  struct Configuration {
      listen: SocketAddr,
      database_url: String,
      #[serde(default = "default_max_media_bytes")]
      max_media_bytes: usize,
      #[serde(default = "default_orphan_media_retention_seconds")]
      orphan_media_retention_seconds: u64,
      #[serde(default)]
      observer_token: Option<String>,
      #[serde(default)]
      connectors: Vec<ConnectorConfiguration>,
      #[serde(default)]
      rules: Vec<Rule>,
      #[serde(default)]
      notify: Vec<ChatRef>,
      #[serde(default)]
      functions: FunctionsConfig,
  }

  #[derive(Deserialize)]
  struct ConnectorConfiguration {
      id: String,
      platform: String,
      #[serde(default)] supports_threads: bool,
      #[serde(default)] supports_topic_create: bool,
  }
  ```

- `crates/wuphf-cli/src/main.rs:124-160` — `serve()` reads the file with
  `tokio::fs::read_to_string`, parses with `toml::from_str`, connects
  `SqliteStorage`, maps each `ConnectorConfiguration` to a
  `wuphf_model::Connector` with `platform(&connector.platform)?` (the
  `platform()` function is at main.rs:250-263), builds
  `Arc::new(RoutingConfig::new(configuration.rules, connectors.clone()).with_notice_targets(configuration.notify))`,
  and resolves `configuration.functions.resolve(|name| std::env::var(name).ok())`.
- `crates/wuphf-cli/src/main.rs:265-381` — three config tests
  (`example_configuration_parses`, `one_to_one_rule_and_filters_parse`,
  `functions_section_parses`), all via `toml::from_str::<Configuration>`.
- `crates/wuphf-functions/src/config.rs:14-16, 52-53` — `FunctionsConfig` and
  `ProviderConfig` derive `Debug, Clone, Deserialize` (no `Serialize`);
  `FunctionOverride` likewise (check its derive line). `resolve(self, env)`
  consumes the config.
- `crates/wuphf-core/src/config.rs:23-41` — `RoutingConfig::new(rules,
  connectors)` and `with_notice_targets(targets)`.
- `wuphf_model::{Rule, ChatRef}` already derive `Serialize, Deserialize`
  (`crates/wuphf-model/src/routing.rs:78, 160`).
- botconf (at the pinned tag) exposes, from the crate root:
  `Schema` (`type Config: DeserializeOwned + Serialize + Send + Sync`,
  `type Derived: Send + Sync`, `fn derive(&self, &Config) -> Result<Derived, String>`,
  `fn settings(&self) -> Vec<RuntimeSetting>`, defaulted `sections`, `lint`,
  `on_change`), `SettingsStore::load(schema, base: Figment, storage) -> Result<SettingsStore<S>, SettingsError>`,
  `SettingsStore::current() -> Arc<Snapshot<S>>` (with `.config` and
  `.derived`), `MemoryStorage`, and `SettingsError`. It depends on
  `figment 0.10` with the `env` and `toml` features.

Conventions: Rust 2024; modules start with `//!`; errors via `thiserror`
(the binary uses `anyhow`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Build | `cargo build --workspace --all-targets` | exit 0 |
| New crate tests | `cargo test -p wuphf-settings` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline (147 at `643829b`) + new tests − the 3 moved CLI tests + their moved copies |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| Dependency boundary | `cargo tree -p wuphf-core --depth 1 \| grep -E "figment\|botconf"` | no output |

## Scope

**In scope**:
- `crates/wuphf-settings/**` (create)
- root `Cargo.toml`, `Cargo.lock`
- `crates/wuphf-cli/Cargo.toml`, `crates/wuphf-cli/src/main.rs`
- `crates/wuphf-functions/src/config.rs` (add `Serialize` derives only)
- `README.md` layout block, `docs/architecture.md` components table,
  `docs/index.html` crate table (documentation only)

**Out of scope** (do NOT touch):
- `crates/wuphf-core` — no dependency on figment or botconf; no API change.
- `crates/wuphf-storage` — plan 036.
- Any runtime key, `on_change` hook, or live routing — plan 037.
- The rules format — plan 038.
- `deny_unknown_fields` on `Configuration` — a behaviour change; see
  Maintenance notes.

## Git workflow

- Commit on the current branch. Do not create a branch, push, or open a PR
  unless the operator instructed it.
- Commits:
  1. `feat: add wuphf-settings with the core configuration schema`
  2. `refactor(cli): load the configuration through wuphf-settings`
  3. `docs: describe wuphf-settings`

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ `147` at `643829b` (record yours).

### Step 1: Create the crate

Add `"crates/wuphf-settings"` to workspace `members` (after
`"crates/wuphf-functions"`), and to `[workspace.dependencies]`:

```toml
botconf = { git = "https://github.com/propheci/botconf", tag = "v0.1.0" }
figment = { version = "0.10", features = ["env", "toml"] }
wuphf-settings = { path = "crates/wuphf-settings" }
```

(Use the tag the prerequisite check found. Match figment's minor version to
the one botconf resolves to — `cargo tree -p botconf | grep figment` — so the
workspace has one figment.)

`crates/wuphf-settings/Cargo.toml`:

```toml
[package]
name = "wuphf-settings"
description = "The core configuration: figment loading and the botconf schema"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
botconf.workspace = true
figment.workspace = true
serde.workspace = true
thiserror.workspace = true
wuphf-core.workspace = true
wuphf-functions.workspace = true
wuphf-model.workspace = true

[dev-dependencies]
figment = { workspace = true, features = ["test"] }
tokio.workspace = true
```

Modules (each starting with a `//!` doc comment):

- `src/lib.rs` — crate doc: what the crate owns (the configuration types, how
  they load, the schema botconf validates them with) and what it must not
  (routing logic stays in `wuphf-core`). Re-exports:
  `pub use config::{Configuration, ConnectorConfiguration, Secret};`
  `pub use load::{ENV_PREFIX, base, load};`
  `pub use schema::{Derived, SettingsStore, Snapshot, WuphfSchema};`
- `src/config.rs` — move `Configuration`, `ConnectorConfiguration`, the two
  `default_*` functions and `platform()` here from `main.rs`, verbatim except:
  - fields become `pub`;
  - derives become `Debug, Clone, Deserialize, Serialize` (both structs);
  - `observer_token: Option<Secret>`;
  - add `Secret` (`#[derive(Clone, Deserialize)] #[serde(transparent)] pub struct Secret(String)`,
    with `expose(&self) -> &str`, a redacting `Debug`, and a manual
    `Serialize` that writes `"<redacted>"`; doc comment says why).
- `src/schema.rs`:

  ```rust
  /// What the core runs from, computed from a validated configuration.
  pub struct Derived {
      pub routing: Arc<RoutingConfig>,
      pub connectors: Vec<Connector>,
      pub functions: Arc<Functions>,
  }

  /// Describes the core configuration to botconf.
  pub struct WuphfSchema {
      /// Reads environment variables (tokens); injectable for tests.
      env: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
  }

  pub type SettingsStore = botconf::SettingsStore<WuphfSchema>;
  pub type Snapshot = botconf::Snapshot<WuphfSchema>;
  ```

  `WuphfSchema::new()` reads the process environment;
  `WuphfSchema::with_env(f)` for tests. `impl botconf::Schema`:
  `type Config = Configuration; type Derived = Derived;`
  `derive` does exactly what `serve()` does today, in the same order, turning
  each failure into a `String`: map connectors with `platform()`, build the
  `RoutingConfig` with notice targets, and
  `config.functions.clone().resolve(|name| (self.env)(name))` (error message
  prefixed `invalid [functions] configuration: `). If plan 034 has landed,
  `derive` also resolves each connector's `token_env` through `self.env`
  (moved out of `serve()`) into a `pub connector_tokens: Arc<ConnectorTokens>`
  field of `Derived`, so every token is read in one place. `settings()` returns
  `Vec::new()` with a comment: "No key changes at runtime yet; plan 037 adds
  `rules` and `notify`."
- `src/load.rs`:

  ```rust
  /// Prefix of environment variables overriding the file, e.g.
  /// `WUPHF_CORE_LISTEN`; `__` separates levels
  /// (`WUPHF_CORE_FUNCTIONS__MAX_DEADLINE_MS`).
  pub const ENV_PREFIX: &str = "WUPHF_CORE_";

  /// The file, then the environment. Fails if the file is missing.
  pub fn base(path: &Path) -> Result<Figment, LoadError>;

  /// The settings store over `base` with `storage` holding the overrides.
  pub async fn load(schema: WuphfSchema, base: Figment, storage: impl botconf::Storage) -> Result<SettingsStore, botconf::SettingsError>;
  ```

  `base` uses `Toml::file_exact(path)` merged with
  `Env::prefixed(ENV_PREFIX).split("__")`, and returns
  `LoadError::NotFound(path)` when `!path.is_file()` (figment's `file_exact`
  errors too, but the explicit message is clearer).

**Verify**: `cargo build -p wuphf-settings` → exit 0.

### Step 2: `Serialize` for the functions section

Add `Serialize` to the derives of `FunctionsConfig`, `ProviderConfig` and
`FunctionOverride` in `crates/wuphf-functions/src/config.rs`. Nothing else
changes there (the token *values* are never in these structs — only variable
names — so serializing them leaks nothing).

**Verify**: `cargo test -p wuphf-functions` → all pass.

### Step 3: Move and extend the configuration tests

Move the three tests from `crates/wuphf-cli/src/main.rs` to
`crates/wuphf-settings/src/tests.rs`, parsing through figment instead of
`toml::from_str`:

- `example_configuration_parses` — `base(Path::new("../../config/example.toml"))`
  (path relative to the crate's manifest dir; use
  `concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/example.toml")`), then
  `.extract::<Configuration>()`; same assertions as today.
- `one_to_one_rule_and_filters_parse`, `functions_section_parses` — use
  `figment::Jail` to write the TOML to a file and call `base`.

New tests:

- `environment_overrides_the_file` — in a `Jail`, set
  `WUPHF_CORE_LISTEN=127.0.0.1:6000` and
  `WUPHF_CORE_FUNCTIONS__MAX_DEADLINE_MS=1234`; the extracted values win.
- `unrelated_wuphf_variables_are_not_read` — set `WUPHF_INVOKE_TOKEN=x` and
  `WUPHF_LOG_LEVEL=debug`; extraction succeeds and nothing changes (guards the
  prefix decision).
- `observer_token_is_redacted_when_serialized` — load a config with
  `observer_token = "s3cret"`; `serde_json::to_string(&config)` does not
  contain `s3cret`, and `config.observer_token.unwrap().expose() == "s3cret"`.
- `store_loads_the_example_and_derives_routing` — `load(WuphfSchema::with_env(…the
  example's token variables…), base(example), botconf::MemoryStorage::new())`;
  `store.current().derived.connectors.len()` equals the example's connector
  count and `derived.routing.rule(&RuleId("wa-to-telegram-mirror".into()))` is
  `Some`.
- `derive_rejects_an_unknown_platform` and
  `derive_rejects_a_provider_without_its_token` — `load` returns
  `SettingsError::InvalidBase` naming the cause.
- `missing_file_is_reported`.

**Verify**: `cargo test -p wuphf-settings` → all pass.

Commit 1.

### Step 4: Switch the CLI over

In `crates/wuphf-cli`:

- `Cargo.toml`: add `wuphf-settings.workspace = true` and
  `botconf.workspace = true`; drop `toml` and `serde` if nothing else uses
  them (check with `cargo build`).
- `serve(path)`:

  ```rust
  let base = wuphf_settings::base(&path).with_context(|| format!("failed to load configuration {}", path.display()))?;
  let settings = Arc::new(
      wuphf_settings::load(WuphfSchema::new(), base, botconf::MemoryStorage::new())
          .await
          .context("invalid configuration")?,
  );
  let snapshot = settings.current();
  let configuration = &snapshot.config;
  let derived = &snapshot.derived;
  ```

  Then use `configuration.listen`, `.database_url`, `.max_media_bytes`,
  `.orphan_media_retention_seconds`, `.observer_token` (via `expose()`),
  `derived.connectors`, `Arc::clone(&derived.routing)` and
  `Arc::clone(&derived.functions)` where the old code used the parsed values.
  Delete the moved structs, `platform()` and the `default_*` functions. Keep
  `settings` alive for the life of `serve()` (plans 036–039 use it); a
  `let _settings = settings;` is not needed if it stays in scope.
- The `--config` default (`config/example.toml`) is unchanged.

**Verify**: `cargo build --workspace --all-targets` → exit 0;
`cargo test --workspace` → all pass, total = Step 0 + new tests;
`cargo run -p wuphf-cli -- serve --config config/example.toml` with the
example's token variables exported → starts and logs
`wuphf connector gRPC server listening` (stop it with Ctrl-C). Then with
`WUPHF_CORE_LISTEN=127.0.0.1:50999` exported → logs `listen=127.0.0.1:50999`.

Commit 2.

### Step 5: Document

- `README.md` layout block: `wuphf-settings/ Core configuration: loading, env overrides, botconf schema`.
- `docs/architecture.md` components table: a `wuphf-settings` row ("Loads the
  core configuration (TOML, then `WUPHF_CORE_*` environment) and describes it
  to botconf; builds the routing config and connector list from it.").
- `docs/index.html`: the matching crate-table row.
- `config/example.toml` header: two comment lines on the environment
  override (`WUPHF_CORE_` + key path in upper case, `__` between levels).

**Verify**: `grep -c "wuphf-settings" README.md docs/architecture.md docs/index.html` → each ≥ 1.

Commit 3.

### Step 6: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0. `cargo tree -p wuphf-core --depth 1 | grep -E "figment|botconf"` →
no output.

## Test plan

In `crates/wuphf-settings/src/tests.rs`: the three moved tests plus
`environment_overrides_the_file`, `unrelated_wuphf_variables_are_not_read`,
`observer_token_is_redacted_when_serialized`,
`store_loads_the_example_and_derives_routing`,
`derive_rejects_an_unknown_platform`,
`derive_rejects_a_provider_without_its_token`, `missing_file_is_reported`.

## Done criteria

- [ ] `cargo test --workspace` exits 0; the tests above exist by name
- [ ] `cargo clippy --workspace --all-targets` and `cargo fmt --check` exit 0
- [ ] `config/example.toml` loads unchanged (no key renamed or moved)
- [ ] `cargo tree -p wuphf-core --depth 1 | grep -E "figment|botconf"` → no output
- [ ] `git diff --stat 643829b..HEAD` lists only in-scope paths
- [ ] `plans/README.md` status row updated

## STOP conditions

- The botconf repository or tag is missing, or its API differs from "Current
  state" (e.g. `SettingsStore::load` takes different arguments) — report the
  actual signatures.
- figment fails to deserialize something `toml::from_str` accepted (likely
  candidates: `ChatRef`'s `#[serde(flatten)]`, `RuleKind`'s internal `kind`
  tag, `SocketAddr`) — report the failing key and error; do not change the
  model types to work around it.
- botconf pulls in a second, incompatible `figment` or a dependency that
  fails on the toolchain (rustc 1.95 in the dev shell).
- Making the CLI compile requires changing `wuphf-core`'s API.

## Maintenance notes

- Plan 036 replaces `MemoryStorage` with the SQLite-backed storage; the
  `database_url` must then be read from `base.extract()` before the store
  loads (the store needs the database).
- Plan 037 gives `derive` the rule validation and makes `rules` and `notify`
  runtime keys.
- `deny_unknown_fields` on `Configuration` would catch typos (botconf
  recommends it for sections) but turns today's silently ignored stray keys
  into startup errors. Do it as its own small change once the owner agrees;
  the `WUPHF_CORE_` prefix keeps it possible.
- `platform()` moves here verbatim; plan 022 (Platform `FromStr` in
  `wuphf-model`) can replace it later.
