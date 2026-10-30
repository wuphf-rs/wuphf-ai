# Plan 056: Runtime settings in the Rust connector SDK; whatsapp-rust declares the Go connector's settings

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Build limits (the owner's laptop)**:
> - Prefix every `cargo` and `go` command with `nice -n 19` and set
>   `CARGO_BUILD_JOBS=2` (Go: `GOMAXPROCS=2 GOFLAGS=-p=2`). Check and test only
>   the crates you touch (`-p <crate>`); never the whole workspace.
> - The whatsapp-rust connector (`connectors/whatsapp-rust`) is its own Cargo
>   workspace and the heaviest build in the repo. Every cargo command for it
>   needs `RUSTC_BOOTSTRAP=1` and `--manifest-path
>   connectors/whatsapp-rust/Cargo.toml`, and it runs alone: never while any
>   other build runs.
> - Before **every** build or test command, run
>   `pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv`.
>   If it prints anything, another build is running: wait, re-checking every
>   60 seconds, until it prints nothing.
> - One build at a time. No `nix flake check`. No release builds except the
>   dev-binary rebuild in the last step.
> - The last step rebuilds the touched dev binaries into
>   `scripts/dev-bin/.bin/`, mirroring `scripts/install-dev-bins.sh`, one at a
>   time and niced, because the owner live-tests with them.
> - Stage explicit paths only (`git add <path> …`; never `git add -A`, `.` or
>   `-u`): another session may be working in the same tree. Run
>   `git status --short` before each commit and commit only this step's files.
>
> **Drift check (run first)**: plans that must be DONE: none for the SDK
> part. **055 is a soft dependency**: it brings `media_convert_timeout_ms`
> and `[sticker_metadata]` (and 054 brings `[rendering]`); Step 4 declares
> whichever of the Go connector's runtime keys the Rust connector has by
> then, and the plan's notes must list any it could not declare. If 054 is
> not DONE either, the sender reads no settings: skip Step 4 item 4 and its
> tests 3–4, and say so in the notes. The intended order is 051–055, then
> this plan. Run
> `git log --oneline 313038d..HEAD -- crates/wuphf-connector crates/wuphf-proto crates/wuphf-runtime/src/connector_settings.rs crates/wuphf-runtime/src/settings_auth.rs connectors/whatsapp-rust connectors/shared/settings connectors/shared/logging connectors/whatsapp/internal/config Cargo.toml`.
> Expected: no commits, or only commits of plans 051–055 that
> `plans/README.md` marks DONE. What must still hold (if any is false, STOP;
> a fact a DONE plan changed as its own plan says counts as holding):
> - `crates/wuphf-connector/src/client.rs:144-156`: the registration frame
>   sends `settings: None` ("This SDK declares no runtime settings yet.");
>   `Options` (38-56) has no settings field.
> - `crates/wuphf-proto/common.proto:36-58`: `Registration.settings` is an
>   `optional ConnectorSettingsDeclaration { bytes descriptor; string
>   json_schema; string base_json }`, the descriptor being a serialized
>   `botconf.v1.DescribeResponse` with keys relative to the section.
> - The core: `crates/wuphf-runtime/src/connector_settings.rs:28-33` accepts
>   only keys under `rendering`, `sticker_metadata`, `log_level`,
>   `media_convert_timeout_ms`; the section is `connector_settings.<id>`
>   (36-39); `crates/wuphf-runtime/src/settings_auth.rs:1-16` lets a
>   connector read and watch its own section with its connector token and no
>   actor.
> - Workspace `Cargo.toml`: `botconf`, `botconf-grpc`, `botconf-panel` at
>   `tag = "v0.10.0"`; tonic 0.12, prost 0.13. `botconf-grpc` exposes
>   `proto::{settings_client::SettingsClient, WatchRequest, WatchEvent {
>   values, initial, catalog_changed }, ValueEntry { key, value_json, .. },
>   DescribeResponse, SettingDescriptor, Kind, TextKind, NumberKind,
>   OneOfKind, Choice }` (botconf repo `proto/botconf/v1/settings.proto`,
>   `rust/botconf-grpc/src/lib.rs:50-53`). After botconf 013 (`v0.11.0`) the same items
>   live in the `botconf-proto` crate (`botconf_proto::…`).
> - `connectors/whatsapp-rust/Cargo.lock` contains no `botconf`, `figment`
>   or `arc-swap` package; tonic 0.12.3, prost 0.13.5.
> - Go shared (the reference): `connectors/shared/settings/settings.go`
>   — `Live` (50-73), `Declare` (78-112), `describe` / `kindOf` (116-233:
>   strings with an enum are one-of with value = label, other strings text,
>   bools one-of `✅ Yes` / `❌ No`, integers numbers bounded by their type),
>   `Watch` (300-327: an initial event starts again from the base; a value
>   that does not decode is reported and skipped), `apply` / `integers` /
>   `present` (330-380); `connection.go:28-61` (`Connect`: declare, dial the
>   settings service with the core token, a watch that reconnects by itself;
>   `Follow`); `logging/logging.go:22-60` (`SetLevel`: `WUPHF_LOG_LEVEL` wins
>   over the configured level, which wins over `info`; `CheckLevel`).
> - Go WhatsApp: `internal/config/config.go:43-80` `Runtime` (the seven
>   tagged settings, their titles and docs), `:119-128` `WithDefaults`,
>   `:163-166` `Validate` checks the log level; `internal/whatsapp/connector.go:71-85`
>   (declare, follow, log level on change, converter timeout read per call);
>   `sender.go:74-93` (`pin`: one dispatch reads the settings once).
> - `connectors/whatsapp-rust/src/main.rs:200-205` `init_logging` builds a
>   fixed `EnvFilter` (`WUPHF_LOG_LEVEL` wins); nothing can change it later.

## Status

- **Priority**: P2 — the Rust connector's settings cannot be changed from
  the settings panel or `wuphf settings`, and the panel shows nothing for it
- **Effort**: L
- **Risk**: MED — the SDK gains a settings client (a new dependency for every
  Rust connector); the logging setup changes
- **Depends on**: — (055 soft)
- **Category**: feature / parity
- **Planned at**: commit `313038d`, 2026-10-31

## Why this matters

The Go connectors declare their runtime settings when they register (plan
040), so the owner changes a log level, a rendering option or the sticker
pack from the Telegram settings panel or `wuphf settings`, and the change
applies at once. The Rust SDK sends `settings: None`; plan 040 deferred the
Rust WhatsApp connector. With the Rust connector at feature parity after
051–055, the last gap is that the same keys cannot be managed the same way,
so the panel and the CLI would treat the two implementations differently.

Plan 6 of 6 (051–056) bringing the Rust connector to parity with the Go one.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The Rust SDK (`crates/wuphf-connector`) gains runtime settings like Go `connectors/shared/settings`: declare at registration, connect to the core's settings service, watch, a live value, follow — including the registration field and the server-pushed values. With its own tests. |
| 2 | A live log level, like Go `logging.SetLevel`. |
| 3 | The Rust WhatsApp connector then declares the same runtime settings as the Go one — `log_level`, `media_convert_timeout_ms`, `sticker_metadata.*`, `rendering.*` — with the same keys and schema, so the panel and the CLI treat both alike. The declaration format follows plans 040 / 044. |

### Planner decisions (within the settled scope)

- **The SDK talks to the settings service through the proto-only
  `botconf-proto` crate** (owner Q48, 2026-10-31). botconf plan 013 creates it
  and tags it `v0.11.0`; `botconf-grpc` re-exports it as `botconf_grpc::proto`,
  so the core keeps its paths. The connectors' locks then gain only
  `botconf-proto`, with no `botconf`, `botconf-panel` or figment. **Prerequisite**:
  botconf 013 is DONE and `git ls-remote --tags https://github.com/propheci/botconf`
  lists `v0.11.0`; otherwise STOP. Step 1 moves the wuphf workspace's three
  botconf pins to `v0.11.0` and adds `botconf-proto` at the same tag.
- **No reflection: a connector lists its settings.** Go reads `botconf`
  struct tags; Rust has no runtime reflection and a derive macro would be a
  new proc-macro crate. A `RuntimeSettings` trait returns the list (key,
  title, description, kind); the declaration's descriptor, schema and base
  are all derived from that list and the base value (`serde`), so each
  setting is written once. Every field of the section must be listed (Go
  reflects untagged fields into the schema too; the WhatsApp section has
  none).
- **Schema parity is checked against a fixture the Go connector writes**
  (as plan 055 does for stickers): the Go test regenerates it with
  `WUPHF_UPDATE_FIXTURES=1` and otherwise checks it; the Rust test compares
  its declaration with it — descriptor messages equal, schemas equal as JSON
  values except the Go-package `$id` (if invopop emits one), bases equal.
- **The live value is pinned per dispatch** (Go `pin`): `send_message` reads
  it once, so all parts of one dispatch see the same settings; a change
  applies from the next dispatch.
- **`WUPHF_LOG_LEVEL` keeps accepting a full `EnvFilter` directive** (as the
  Rust connector does today, `main.rs:200-205`) and still wins over the
  configured level on every change; a configured level must be one of
  trace, debug, info, warn, error (any case; empty = info), checked when the
  file loads — the core refuses a declaration whose level it does not know
  (Go `CheckLevel`).

## Current state

See the drift check. Also:

- The core turns a declaration into a botconf section at
  `connector_settings.<id>`, validates every change against the JSON Schema
  (`connector_settings.rs:296-330`, which also coerces numbers), stores the
  declaration (`connector_settings_declarations`, migration 0018) and
  serves the section through `Watch` (`docs/architecture.md:150-160`).
- Go's tests: `connectors/shared/settings/settings_test.go`
  (`TestDeclareListsOnlyTaggedFields`, `TestDeclareSchemaUsesTomlNames`,
  `TestLiveSwapIsAtomic`, `TestWatchAppliesPushedValues`,
  `TestWatchSkipsValuesThatDoNotDecode`), `logging/logging_test.go`,
  `connectors/whatsapp/internal/config/config_test.go`
  (`TestRuntimeSettingsKeepTheFileLayout`, `TestRuntimeDefaultsAreExplicit`,
  …), `internal/whatsapp/runtime_settings_test.go`
  (`TestStickerMetadataChangeAppliesToNextSticker`),
  `connectors/shared/coreclient/client_test.go`
  (`TestRegistrationCarriesTheSettingsDeclaration`).
- After 054 the Rust sender holds `runtime: Runtime` and clones it at the
  start of `send_message`; after 055 the converter's timeout closure reads a
  captured `Runtime`.

## Commands you will need

```sh
pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv   # must print nothing

export CARGO_BUILD_JOBS=2
nice -n 19 cargo test -p wuphf-connector
nice -n 19 cargo clippy -p wuphf-connector --all-targets -- -D warnings
cargo fmt --check

(cd connectors/whatsapp && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go vet ./... && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go test ./internal/config/)

RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo test --manifest-path connectors/whatsapp-rust/Cargo.toml
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --manifest-path connectors/whatsapp-rust/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path connectors/whatsapp-rust/Cargo.toml --check
```

`botconf-proto` compiles its proto with `tonic-build`, so `protoc` must be on
the `PATH` for the whatsapp-rust build too (it is in the nix dev shell, as
for `wuphf-proto`).

## Scope

In: `crates/wuphf-connector/{Cargo.toml,src/lib.rs,src/client.rs,src/settings.rs (new),src/logging.rs (new),tests/settings.rs (new),tests/session.rs}`,
`Cargo.lock` (dependency edges only),
`connectors/whatsapp/internal/config/{config_test.go,testdata/ (new fixture)}`,
`connectors/whatsapp-rust/{Cargo.lock,config.example.toml,src/main.rs,src/config.rs,src/sender.rs}`,
`README.md` (Rust connector section), `plans/README.md`.

Out: the core (it already accepts declarations), the botconf repo, the Go
connector's code (only a test and a fixture are added), the Telegram panel,
settings that are not runtime keys (identity, addresses, session files,
capabilities stay file-only, as plan 040 settled), `.env.local`,
`config.local.toml`, `*.session.json`, `.claude/`,
`connectors/instagram/**`, `plans/048-*`.

## Git workflow

Commit directly on `v2`, one commit per step. Do not push. No branches.

## Steps

### Step 0: Baseline

Drift check. Record `cargo test -p wuphf-connector`, the whatsapp-rust test
count (alone), and `go test ./internal/config/` in `connectors/whatsapp`.
Note which of `[rendering]`, `media_convert_timeout_ms` and
`[sticker_metadata]` the Rust config has (054 / 055 done or not).

### Step 1: SDK logging with a live level

`crates/wuphf-connector/Cargo.toml`: `tracing-subscriber.workspace = true`
(the workspace enables `env-filter`). New `src/logging.rs`, exported from
`lib.rs` (Go `connectors/shared/logging`):

```rust
//! The connector's log subscriber, with a level that can change while it
//! runs (the log level is a runtime setting). `WUPHF_LOG_LEVEL` overrides the
//! configured level, on every change too, without editing the file.

pub const ENV_LEVEL: &str = "WUPHF_LOG_LEVEL";
/// The levels a configuration may name (the runtime settings declare the
/// same list).
pub const LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// An error for a configured level other than the five (any case); empty
/// means info.
pub fn check_level(configured: &str) -> Result<(), String>;

/// The filter in force: the environment's directive when it parses, else the
/// configured level when it is one of the five, else info.
pub fn effective_filter(env: Option<&str>, configured: &str) -> EnvFilter;

/// Changes the level of the installed subscriber.
#[derive(Clone)]
pub struct LogLevel { handle: reload::Handle<EnvFilter, Registry> }

/// Install the global fmt subscriber with a reloadable filter.
pub fn init(configured: &str) -> LogLevel;

impl LogLevel {
    /// Make `configured` the level, the environment still winning.
    pub fn set(&self, configured: &str);
}
```

Tests (`logging.rs`): `the_environment_wins_then_the_config_then_info`
(compare `effective_filter(..).to_string()`: env `debug` over `warn`; no env
→ `warn`; an invalid configured level → `info`; an env directive
`whatsapp_rust=debug,info` kept as it is); `only_the_five_levels_are_accepted`
(`INFO` and `""` fine, `verbose` refused, the error lists the five) (Go
`logging_test.go`).

**Verify**: `nice -n 19 cargo test -p wuphf-connector` → Step 0 count + 2;
clippy clean; `git diff Cargo.lock` → only `wuphf-connector`'s dependency
list.
Commit: `feat(connector-sdk): logging with a live level`.

### Step 2: SDK runtime settings

`crates/wuphf-connector/Cargo.toml`: `botconf-proto.workspace = true`, and
`serde.workspace = true`, `serde_json.workspace = true` if 055 has not added
them. New `src/settings.rs`, exported from `lib.rs`:

```rust
//! Runtime settings for Rust connectors: the counterpart of the Go
//! `connectors/shared/settings` package (plan 040). A connector keeps the
//! settings that can change while it runs in one serde type, lists them, and
//! declares them when it registers; the core stores and validates every
//! change and pushes it back through its settings service, and the connector
//! reads the live value once per use.

pub const NAMESPACE: &str = "connector_settings";
pub fn prefix(connector_id: &str) -> String;  // "connector_settings.<id>"

/// One runtime setting (Go: a `botconf`-tagged field).
pub struct Setting {
    pub key: &'static str,          // dotted, relative to the section: "rendering.max_message_length"
    pub title: &'static str,
    pub description: &'static str,
    pub kind: SettingKind,
}
pub enum SettingKind {
    Text,
    /// One of these values (each its own label).
    Choice(&'static [&'static str]),
    /// Offered as `✅ Yes` / `❌ No`.
    Bool,
    Number { min: i64, max: i64 },
}

/// A connector's runtime section.
pub trait RuntimeSettings: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// Every setting of the section, in the order the panel lists them.
    fn settings() -> Vec<Setting>;
}

/// The registration's declaration: a botconf descriptor of the settings, a
/// JSON Schema of the section (draft 2020-12, nested objects by key, no
/// unknown properties), and `base` as the section's values.
pub fn declare<T: RuntimeSettings>(base: &T) -> Result<pb::ConnectorSettingsDeclaration, SettingsError>;

/// The current value; readers load it once per use.
pub struct Live<T> { value: RwLock<Arc<T>> }
impl<T> Live<T> { pub fn new(initial: T) -> Self; pub fn load(&self) -> Arc<T>; pub fn set(&self, value: T) -> Arc<T> /* the old one */; }

/// Lays one watch event over `current` (an initial event over `base`):
/// values without JSON and keys outside `prefix` are ignored; whole floats
/// become integers (Go `integers`).
pub fn apply<T: RuntimeSettings>(current: &T, base: &T, event: &WatchEvent, prefix: &str) -> Result<T, SettingsError>;

/// The declaration, the live value, and a client of the core's settings
/// service (Go `settings.Connection`).
pub struct Connection<T> { pub declaration: pb::ConnectorSettingsDeclaration, pub live: Arc<Live<T>>, /* base, prefix, client, authorization */ }
impl<T: RuntimeSettings> Connection<T> {
    /// Declare `base` and set up a lazy connection with the core token.
    pub fn connect(core_address: &str, token: &str, connector_id: &str, base: T) -> Result<Self, SettingsError>;
    /// Apply the core's pushes until `cancel` fires. After each (re)connect
    /// the core sends the whole section; the stream is re-opened with
    /// backoff (1 s doubling to 30 s) when it ends. `on_change(old, new)`
    /// runs after each swap. A push that does not decode is logged at warn
    /// and skipped: the connector keeps its last value.
    pub async fn follow(&self, cancel: CancellationToken, on_change: impl Fn(&T, &T) + Send + Sync);
}
```

`Options` gains `pub settings: Option<pb::ConnectorSettingsDeclaration>`
(documented like Go `coreclient.Options.Settings`), sent in the registration
frame instead of `None` (`client.rs:155`). Update the `Options` literals in
`tests/session.rs` (144, 342, 416) with `settings: None` and the
whatsapp-rust `main.rs` literal (Step 4 fills it).

Descriptor details (Go `kindOf`): `Choice` → `OneOfKind` with
`Choice { value, label: value }`; `Bool` → `OneOfKind` with `("true", "✅
Yes")`, `("false", "❌ No")`; `Number` → `NumberKind { min, max }`; `Text` →
`TextKind {}`; `position` 0 and `per_entry` false, as Go leaves them.

Tests (`settings.rs` unit tests and `tests/settings.rs`):

1. `declare_lists_the_settings_with_their_kinds` — a test section with a
   choice, a bool, a bounded number and a nested text → keys in order,
   titles, descriptions, choices, bool labels, bounds (Go
   `TestDeclareListsOnlyTaggedFields`).
2. `the_schema_nests_dotted_keys` — objects per prefix, `additionalProperties:
   false`, the enum, `string` / `boolean` / `integer` (Go
   `TestDeclareSchemaUsesTomlNames`).
3. `the_base_is_the_values` — `base_json` equals `serde_json::to_value(base)`.
4. `a_first_event_starts_from_the_base` — current differs from base; an
   initial event with one value → base plus that value.
5. `later_events_change_only_their_keys`.
6. `whole_floats_decode_into_integers` — `"20000.0"` into a `u32`.
7. `an_undecodable_push_is_refused` — a string for a number → `Err`, and
   `follow` keeps the last value (covered in 9).
8. `values_without_json_or_outside_the_prefix_are_ignored`.
9. `follow_applies_the_core_s_pushes` (`tests/settings.rs`) — a fake
   `botconf_proto::settings_server::Settings` server in the test
   (`watch` streams scripted events; every other RPC returns
   `unimplemented`) → the request carried `authorization: Bearer <token>`,
   no actor and the prefix `connector_settings.<id>`; after an initial and a
   change event, `live.load()` has both; `on_change` saw each swap; an
   undecodable event left the value unchanged.
10. `follow_reconnects_when_the_stream_ends` — the fake ends the stream
    after one event and serves a second connection → the second initial
    event is applied (from the base).
11. `the_registration_carries_the_declaration` (`tests/session.rs`, pattern
    `the_registration_carries_the_disappearing_capability`; Go
    `TestRegistrationCarriesTheSettingsDeclaration`).
12. `live_swaps_whole_values` — `set` returns the old value; concurrent
    `load`s see either value, never a mix (Go `TestLiveSwapIsAtomic`).

**Verify**: `nice -n 19 cargo test -p wuphf-connector` → + 12; clippy clean;
`git diff Cargo.lock` → only dependency-list edges (no new package; botconf
0.10.0 is already locked for the core). Mutations: skip the base on an
initial event → test 4 fails; drop the `authorization` header → test 9
fails. Restore.
Commit: `feat(connector-sdk): declare and follow runtime settings`.

### Step 3: The Go connector's declaration as a fixture

`connectors/whatsapp/internal/config/config_test.go`:
`TestRuntimeDeclarationMatchesTheSharedFixture` — `settings.Declare(Runtime{}.WithDefaults())`;
encode `{"descriptor_hex": <hex of the descriptor bytes>, "json_schema":
<the schema as a JSON object>, "base": <the base as a JSON object>}` with
indentation; with `WUPHF_UPDATE_FIXTURES=1` write it to
`internal/config/testdata/runtime-declaration.json`, otherwise compare with
the file. Generate it, run again without the variable, then **read the
fixture** and note in the commit body whether the schema has `$id`,
`$schema`, and any keywords beyond `type` / `properties` / `enum` /
`additionalProperties` (the Rust generator must emit the same).

**Verify**: `go vet ./...` and `go test ./internal/config/` green;
`git status --short connectors/whatsapp` → the test file and the one new
fixture.
Commit: `test(whatsapp): the runtime settings declaration as a shared fixture`.

### Step 4: whatsapp-rust declares and follows its runtime settings

1. `config.rs`: `log_level` moves into `Runtime` (still a top-level key: the
   struct is flattened); `Runtime` derives `Serialize`; `Runtime::with_defaults()`
   (Go `WithDefaults`: lower-case level, empty → `info`; blank sticker
   fields → the defaults, if 055 is DONE). `Config::load` calls
   `wuphf_connector::logging::check_level(&runtime.log_level)`.
2. `impl RuntimeSettings for Runtime` — the Go tags, word for word
   (`connectors/whatsapp/internal/config/config.go:50-79`), in Go's field
   order:
   | key | title | description | kind |
   |---|---|---|---|
   | `log_level` | Log level | How much the connector logs (WUPHF_LOG_LEVEL wins) | `Choice(LEVELS)` |
   | `media_convert_timeout_ms` | Sticker conversion timeout (ms) | How long converting a sticker may take, waiting for the converter included; 0 means 20000 | `Number { 0, u32::MAX }` |
   | `sticker_metadata.pack_name` | Sticker pack name | The pack name WhatsApp shows for bridged stickers | `Text` |
   | `sticker_metadata.author_name` | Sticker author | The author WhatsApp shows for bridged stickers | `Text` |
   | `sticker_metadata.link` | Sticker link | A link folded into the sticker's publisher | `Text` |
   | `rendering.attachments_as_files` | Attachments as files | Send every attachment as a file, whatever its kind | `Bool` |
   | `rendering.max_message_length` | Longest inline message | Longer text is sent as a file; 0 means no limit | `Number { 0, u32::MAX }` |
   A key whose field the Rust `Runtime` does not have yet (054 / 055 not
   DONE) is left out, and the fixture test (below) is marked `#[ignore]`
   with a reason naming the missing plan — record that in the plan notes.
3. `main.rs`: `let log = wuphf_connector::logging::init(&runtime.log_level)`
   replaces `init_logging`; `Connection::connect(&core_address, &core_token,
   &connector_id, config.runtime.with_defaults())`; `Options.settings =
   Some(connection.declaration.clone())`; spawn
   `connection.follow(cancel.clone(), move |old, new| if old.log_level !=
   new.log_level { log.set(&new.log_level) })`; the sender gets
   `Arc::clone(&connection.live)`; the converter's timeout closure reads
   `live.load().media_convert_timeout()`.
4. `sender.rs`: `runtime: Arc<Live<Runtime>>`; `send_message` pins `let
   runtime = self.runtime.load();` once at the top and every part reads
   that (Go `pin`).
5. `config.example.toml`: mark the runtime keys with the Go example's
   "Runtime." prefix and its opening paragraph (changes made in the panel or
   `wuphf settings` under `connector_settings.<connector id>` win over the
   file until unset).

Tests:

1. `the_declaration_matches_the_go_connector` (`config.rs`; the fixture via
   `include_str!("../../whatsapp/internal/config/testdata/runtime-declaration.json")`):
   decode `descriptor_hex` into `botconf_proto::DescribeResponse` and
   compare with the Rust descriptor decoded the same way; compare the
   schemas as `serde_json::Value` (removing `$id` from Go's when present);
   compare the bases. If the schemas differ, make the SDK's generator emit
   what the fixture has (Step 3's note), not the other way round.
2. `the_log_level_is_checked` — `VERBOSE` refused at load; `INFO` → `info`
   after `with_defaults`; empty → `info`.
3. `a_changed_setting_applies_to_the_next_dispatch` (`sender.rs`, 054's
   harness) — send with `max_message_length = 0` (inline), `live.set(..)`
   with 5, send the same 10-character text → a `message.txt` document (and,
   if 055 is DONE, the sticker pack name case of Go
   `TestStickerMetadataChangeAppliesToNextSticker`).
4. `one_dispatch_sees_one_value` — three attachments; the fake client flips
   `attachments_as_files` in `live` after the first upload → all three parts
   use the first value.

**Verify**: whatsapp-rust tests → + 4, green; clippy clean. Record the new
packages in `connectors/whatsapp-rust/Cargo.lock` (`git diff --stat` and the
`[[package]]` names): expected `botconf-proto` only; **no** second version of tonic,
prost, tokio or serde. Mutation: read `self.runtime.load()` per part
instead of once → test 4 fails. Restore.
Commit: `feat(whatsapp-rust): declare the Go connector's runtime settings and follow them live`.

### Step 5: Docs

- `README.md`, Rust connector section: its runtime settings (the same keys
  as the Go connector's) can be changed from the settings panel and `wuphf
  settings` under `connector_settings.<connector id>`, and apply without a
  restart.
- `docs/architecture.md` (the connector-settings paragraph near 150-160):
  one sentence that the Rust SDK declares and follows settings like the Go
  one.

Commit: `docs: runtime settings for the Rust WhatsApp connector`.

### Step 6: Gate, dev binary, index, and the owner's live test

1. Gate: `cargo test -p wuphf-connector`, clippy, `cargo fmt --check`;
   `connectors/whatsapp` `go vet ./...`, `go test ./...`; alone, the
   whatsapp-rust test, clippy, fmt.
2. Dev binary (check `pgrep` first). The core and the Go connector binaries
   are unchanged:
   ```sh
   RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo build --release --manifest-path connectors/whatsapp-rust/Cargo.toml
   install -m 0755 connectors/whatsapp-rust/target/release/wuphf-connector-whatsapp scripts/dev-bin/.bin/wuphf-whatsapp-rust
   ```
3. `plans/README.md`: row 056 DONE with notes (counts, mutations, the new
   whatsapp-rust lock packages, any key not declared). Commit:
   `docs(plans): mark plan 056 done`.

**For the owner:**

- **Restart**: the whatsapp-rust connector (`wwr`). The core picks the
  declaration up when the connector registers; no core restart.
- **Config**: none new. `log_level` must now be one of trace, debug, info,
  warn, error (any case) or the connector refuses to start; a
  `WUPHF_LOG_LEVEL` directive still wins.
- **Live test**:
  1. Open the settings panel (`/wuphf` to the helper bot) → connector
     settings → `whatsapp-rust`: the same seven settings as `whatsapp-main`,
     with the same titles and choices.
  2. Set "Longest inline message" to 20; from Telegram send a 30-character
     message into a chat mirrored to this connector → it arrives as
     `message.txt`, with no restart. Unset it → the next one is inline.
  3. Set "Log level" to debug → the connector log turns verbose at once; set
     it back to info.
  4. Set "Sticker pack name" to something new and send a sticker → its
     sticker info shows the new name (055).
  5. CLI alternative: `wuphf-server settings set
     connector_settings.whatsapp-rust.rendering.max_message_length 20`, then
     `WUPHF_OBSERVER_TOKEN=<observer token> wuphf-server settings reload` so
     the running core pushes it (use your connector id if it is not
     `whatsapp-rust`).

## Test plan

| Test | Where | Proves |
|---|---|---|
| level precedence, accepted levels | wuphf-connector logging | decision 2 |
| descriptor, schema, base, initial / later events, integers, refused pushes, ignored values, follow with auth and prefix, reconnect, registration, live swap | wuphf-connector settings | decision 1 |
| Go writes and checks the declaration fixture | Go whatsapp config | decision 3 |
| declaration equals Go's, level check, next-dispatch change, one value per dispatch | whatsapp-rust config, sender | decision 3 |

## Done criteria

- [ ] Every test above passes; every listed mutation fails a test.
- [ ] The Rust declaration equals the Go fixture (or the plan notes say which
      keys wait for 054 / 055).
- [ ] The main lock gains no package; the whatsapp-rust lock gains only the
      botconf crates and their dependencies, with no duplicate tonic, prost,
      tokio or serde.
- [ ] Dev binary installed; row 056 DONE with notes and the owner's steps.

## STOP conditions

- The drift check fails, or a "must still hold" fact is false.
- Adding `botconf-proto` to the SDK puts a second version of tonic, prost,
  tokio or serde into either lock file, or `v0.11.0` is not tagged.
- The core refuses the Rust declaration in the live test ("settings
  declaration" errors in the core log) although the fixture test passes:
  report the core's error.
- The Go fixture's schema uses JSON Schema keywords the core's validator
  rejects, or that cannot be produced from the setting list without
  per-type special cases you would have to invent: report the fixture.
- Any test that existed at Step 0 starts failing for a reason this plan does
  not explain.

## Maintenance notes

- **A new runtime key needs three edits**: the Rust `Runtime` field, its
  `Setting` in `RuntimeSettings::settings()`, and the core's `LIVE_SAFE`
  list if it is a new top-level key; on the Go side, the tagged field. The
  shared fixture test fails until both connectors agree.
- **Pushes can race the registration**: the follow loop may connect before
  the core has registered the section; the core then sends a later event
  when the section appears (as for the Go connectors). Nothing to do.
- **The SDK depends on `botconf-proto` only**, so a Rust connector never links
  the settings engine or the panel.
- **`WUPHF_LOG_LEVEL` wins on every change**, so a level set in the panel has
  no effect while the variable is set (as in Go).
