# Plan 040: Connectors declare their own settings to the core

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-proto crates/wuphf-runtime/src/grpc.rs crates/wuphf-runtime/src/connector.rs crates/wuphf-storage crates/wuphf-cli connectors/shared connectors/telegram/internal connectors/whatsapp/internal connectors/instagram/internal proto/go`
> Plans 034–039 are expected to have touched the core files. What must still
> hold: the first `ClientFrame` of a connector session is a `Registration`
> (`crates/wuphf-proto/common.proto:23-27`, sent by
> `connectors/shared/coreclient/client.go:198-207`); the core's settings live
> in a `botconf::SettingsStore<WuphfSchema>` (035) persisted through the sqlx
> storage (036); the settings service is mounted with connector-token
> authorization (039). Otherwise STOP.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED — touches the connector registration protocol and every Go
  connector's sender; a bad push could mis-render every mirrored message
- **Depends on**: 039; botconf plans 005 (`Watch`) and 006 (Go client), plus
  the botconf **dynamic sections** capability (botconf plan 009) described under "Prerequisite"
- **Category**: direction
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

Each Go connector has rendering knobs (Telegram's edit and delete modes,
attachments as files, the inline text limit, WhatsApp's sticker pack
metadata, log levels) that today only change by editing the connector's TOML
and restarting it. The owner wants them editable from the same settings
panel as the core's own settings. The core stays the one store (AGENTS.md:
the Rust core is authoritative), so connectors **declare** their settings to
the core, the core stores and validates every change, and pushes the
effective values back to the connector, which applies them live.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | One store, in the core. Connectors hold no settings database. |
| 2 | At registration a connector sends a **descriptor** of its runtime settings (botconf `RuntimeSetting`s with kinds) plus a **JSON Schema** of the section. The core validates every value with a JSON Schema validator, because it cannot deserialize a Go type. |
| 3 | Only **live-safe** keys are runtime keys: `rendering.*`, `sticker_metadata.*`, `log_level`, `media_convert_timeout_ms`. Identity, addresses, session files, `client_mode`, capabilities and tokens stay file-only (Q14: a runtime key applies immediately; restart-only keys are a later flag). |
| 4 | The core pushes effective values to the connector over botconf's `Watch` stream; the connector swaps them in atomically. |
| 5 | The connector's own TOML stays the **base** of its settings; core-stored overrides win; unsetting returns to the connector's file value. |

### Decisions made by this plan (flagged for the owner)

| # | Decision | Why |
|---|---|---|
| F1 | Settings live at `connector_settings.<connector_id>.*`, not `connectors.<id>.*`. | `connectors` is a TOML array (`[[connectors]]`) holding file-only identity (id, platform, `owner_ids`, token); a path into an array element is not addressable, and mixing runtime and file-only keys in one table invites mistakes. |
| F2 | The core config file may **not** contain `[connector_settings]`; the base values come only from what the connector reports. | Two file bases for one key (the connector's TOML and the core's) would make "unset" ambiguous. |
| F3 | The descriptor travels as a new `Registration` field holding the botconf `SectionDescriptor` message **serialized to bytes**, plus the schema as a JSON string. | wuphf's proto then needs no import of botconf's `.proto` (no cross-repo include path); both ends decode the bytes with botconf's own generated types. |
| F4 | The Rust validator is the `jsonschema` crate (MIT), draft 2020-12, compiled once per registration. | Most widely used Rust implementation, supports `$ref`-free schemas like these trivially. **Its MSRV is unverified offline**: Step 3 checks it builds on rustc 1.95; fall back to `boon` (MIT/Apache-2.0) if not. |
| F5 | The Go schema is generated from struct tags by `github.com/invopop/jsonschema` (MIT), with `toml` names as property names. | Keeps the Go config struct the single description; no hand-written schema to drift. |
| F6 | The Rust `whatsapp-rust` connector is **deferred**: it declares nothing and keeps file-only config. | It is a separate workspace with its own toolchain quirks (RUSTC_BOOTSTRAP); a connector that declares nothing simply has no section, which this plan must support anyway. |
| F7 | The core remembers each connector's last descriptor and schema in SQLite (migration) so the sections exist at startup, before the connector reconnects, and the `wuphf settings` CLI (042) can edit them while connectors are down. | Otherwise stored overrides for a connector would be "unknown keys" and be ignored on every restart until it registers. |

## Prerequisite: botconf dynamic sections

botconf's sections today are static (`SectionSettings::of::<T>(&'static
[RuntimeSetting])`, a `fn` parser, a `Catalog` fixed at `SettingsStore::load`,
"positions never change while the program runs" — `keys.rs`). This plan needs,
from the botconf tag it depends on:

- an owned section form: `SectionSettings::dynamic(runtime:
  Vec<OwnedRuntimeSetting>, validate: Arc<dyn Fn(&Value) -> Result<Value,
  String> + Send + Sync>)` (owned keys, titles, kinds);
- `SettingsStore::upsert_section(Section)` and `remove_section(id)`, which
  rebuild the catalog, revalidate stored overrides under that path (moving
  now-invalid ones to `ignored`), and publish a change that `Watch` reports;
- a figment provider slot per section for its base values (decision 5),
  shown by `Snapshot::source` as `Source::Other("connector <id>")`.

botconf plans 001–008 do not list this yet. **If the tagged botconf lacks
it, STOP** and report that a botconf plan must add it first.

## Current state

- `crates/wuphf-proto/common.proto:23-27`:

  ```proto
  message Registration {
    string connector_id = 1;
    Platform platform = 2;
    Capabilities capabilities = 3;
  }
  ```

- `crates/wuphf-runtime/src/grpc.rs:28-55` — `open_session` reads the first
  frame, converts it with `registration_from_proto`
  (`grpc.rs:91-104`), and calls `hub.connect(registration)`.
- `crates/wuphf-runtime/src/connector.rs:96-110` — `ConnectorHub::register`
  rejects a registration that differs from the configured `[[connectors]]`
  entry (`self.allowed.get(&connector_id) != Some(&request.registration)`).
  The descriptor must **not** be part of that equality: compare identity and
  capabilities only.
- `connectors/shared/coreclient/client.go:57-67` — `Options` (core address,
  connector id, platform, capabilities, `IsPermanent`); `:198-207` sends the
  `Registration`.
- Connector configs (all `pelletier/go-toml/v2`, loaded once in `Load`):
  - Telegram `connectors/telegram/internal/config/config.go:13-48`:
    `LogLevel`, `MediaConvertTimeoutMs`, `Rendering{AttachmentsAsFiles,
    MaxMessageLength, EditMode, DeleteMode}`; `Validate()` checks the mode
    strings (`:106-118`).
  - WhatsApp `connectors/whatsapp/internal/config/config.go:12-58`:
    `LogLevel`, `ClientMode` (file-only), `Stickers StickerMetadata{PackName,
    AuthorName, Link}` (`toml:"sticker_metadata"`), `Rendering{AttachmentsAsFiles,
    MaxMessageLength}`, `MediaConvertTimeoutMs`; defaults filled by
    `StickerMetadata()` (`:83-95`).
  - Instagram `connectors/instagram/internal/config/config.go:11-36`:
    `LogLevel`, `BackfillOnStart` (read at connect only — **not** live-safe,
    stays file-only).
- Senders copy the options by value at construction:
  - Telegram `sender.go:42` `NewSender(…, rendering config.RenderingOptions, bot bool)`,
    read as `s.rendering.X` at `:96-99`, `:129`, `:155`.
  - WhatsApp `sender.go:45` `NewSender(…, stickerMeta, rendering)`, read at
    `:68`, `:72`, `:111`, `:400`.
  - The `functions.MediaConverter{… Timeout: c.cfg.MediaConvertTimeout()}` is
    built once in each `connector.go` (Telegram `connector.go:48`).
- Logging: `connectors/shared/logging/logging.go` builds the root logger with
  `logger.Level(...)`; derived loggers are copies, so a level change needs
  `zerolog.SetGlobalLevel` with the root built at `TraceLevel`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Rust build/test | `cargo test --workspace` | exit 0; baseline + new |
| Lint | `cargo clippy --workspace --all-targets && cargo fmt --check` | exit 0 |
| Go bindings | the `protoc` command in plan 032 Step 2, unchanged | only `common.pb.go` changes |
| Go tests | `for m in proto/go connectors/shared connectors/telegram connectors/whatsapp connectors/instagram; do (cd $m && go vet ./... && go test -race ./...); done` | all exit 0 |
| MSRV check | `cargo +1.95 build -p wuphf-runtime` (or the nix shell's rustc 1.95) | exit 0 |

## Scope

**In scope**:
- `crates/wuphf-proto/common.proto` (`Registration` field), `proto/go/connector/v1/common.pb.go`
- `crates/wuphf-runtime/src/grpc.rs`, `connector.rs` (accept and forward the
  declaration), a new `crates/wuphf-runtime/src/connector_settings.rs`
- `crates/wuphf-storage` (table + migration + `migration_tests.rs` case),
  the `WuphfSchema` module from 035
- `connectors/shared/settings` (create: Go package), `connectors/shared/coreclient`
- the three Go connectors' `config`, `connector.go`, `sender.go`
- docs: `docs/connector-protocol.md`, each connector README / example config

**Out of scope**:
- `connectors/whatsapp-rust` (F6).
- Panels (041) and the CLI (042).
- Restart-required runtime keys.

## Git workflow

- Commit on the current branch. Commits:
  1. `feat(proto): carry a settings declaration in Registration`
  2. `feat(storage): remember connector settings declarations`
  3. `feat(runtime): register connector settings sections`
  4. `feat(connectors): declare and apply live settings`
  5. `docs: describe connector-declared settings`
- Do NOT push.

## Steps

### Step 0: Baseline

Record `cargo test --workspace` total and that every Go module's tests pass.

### Step 1: Protocol

Add to `Registration`:

```proto
// Optional. The connector's runtime settings: a botconf.v1.SectionDescriptor
// serialized with protobuf, and a JSON Schema (draft 2020-12) of the whole
// section. Absent means the connector has no runtime settings.
optional ConnectorSettingsDeclaration settings = 4;

message ConnectorSettingsDeclaration {
  bytes descriptor = 1;
  string json_schema = 2;
  // The connector's own file values for the section, as JSON: the base the
  // core's overrides are layered on.
  string base_json = 3;
}
```

Regenerate Go bindings; only `common.pb.go` may change.

**Verify**: `cargo build -p wuphf-proto` → exit 0; `git status --short proto/go`
→ only `common.pb.go` modified.

Commit 1.

### Step 2: Persist declarations

Migration (next free number after 036's): table
`connector_settings_declarations(connector_id TEXT PRIMARY KEY, descriptor
BLOB NOT NULL, json_schema TEXT NOT NULL, base_json TEXT NOT NULL, updated_at
INTEGER NOT NULL)`. Add a small role trait in `wuphf-storage`
(`SettingsDeclarationStore`: `put_declaration`, `declarations`), implemented
for SQLite and `InMemoryStorage`, and add it to the `Storage` supertrait list.
Add the parity case to `crates/wuphf-storage/src/tests.rs` and a
`migration_tests.rs` case that migrates an empty database and inserts a row.

**Verify**: `cargo test -p wuphf-storage` → exit 0.

Commit 2.

### Step 3: Register sections in the core

In `crates/wuphf-runtime/src/connector_settings.rs`:

1. `fn section_for(connector_id, declaration) -> Result<Section, String>`:
   decode the descriptor bytes with botconf's generated type; reject keys that
   are not live-safe (decision 3's allow list, checked against
   `rendering.`, `sticker_metadata.`, `log_level`, `media_convert_timeout_ms`
   prefixes); compile the schema with `jsonschema` (F4); build
   `SectionSettings::dynamic(...)` whose validator runs the schema and
   returns the value unchanged; path `connector_settings.<id>`; title
   `"<id> (<platform>)"`.
2. At startup (before serving), load every stored declaration and
   `upsert_section` it, so overrides validate before connectors return (F7).
3. On registration (`grpc.rs`, after `hub.connect` succeeds): if a declaration
   is present, store it and `upsert_section`; if it is absent and a section
   exists, keep the stored one (a connector built without settings support
   must not wipe the owner's overrides). Reject (`invalid_argument`, before
   `hub.connect`) a declaration with non-live-safe keys or an uncompilable
   schema.
4. `WuphfSchema::derive` rejects any `connector_settings` value present in
   the core's own file (F2).

Check F4's crate on rustc 1.95 before writing the validator.

**Verify**: runtime tests in the test plan pass;
`cargo +1.95 build -p wuphf-runtime` → exit 0.

Commit 3.

### Step 4: The Go side

Create `connectors/shared/settings`:

```go
// Live holds a connector's current runtime settings. Readers call Load on
// every use; Watch swaps in each new value pushed by the core.
type Live[T any] struct{ value atomic.Pointer[T] }

func NewLive[T any](initial T) *Live[T]
func (l *Live[T]) Load() T

// Declare describes T's runtime fields for Registration: the botconf
// descriptor (from `botconf:"…"` struct tags: title, description, kind,
// choices) and a JSON Schema generated from the `toml` names (F5), plus the
// base values as JSON.
func Declare[T any](base T) (*wuphfpb.ConnectorSettingsDeclaration, error)

// Watch follows the core's pushes for this connector's section through the
// botconf Go client (botconf plan 006) and stores each validated value. It
// returns when ctx ends; on a stream error it reconnects with backoff.
func Watch[T any](ctx context.Context, client *botconf.Client, connectorID string, live *Live[T], onChange func(old, new T)) error
```

`coreclient.Options` gains `Settings *wuphfpb.ConnectorSettingsDeclaration`,
sent in the `Registration`.

Per connector, split the config into a file-only part and a `Runtime` struct
holding only decision 3's keys (Telegram: `LogLevel`, `MediaConvertTimeoutMs`,
`Rendering`; WhatsApp: the same plus `Stickers`; Instagram: `LogLevel`). The
TOML layout does not change: `Runtime` is embedded with `toml:",inline"` or
the existing field names are kept so `config.example.toml` still parses
(verify with the existing config tests). Then:

- senders take `*settings.Live[config.Runtime]` and call `Load()` once per
  dispatch (not per field, so one dispatch sees one consistent value);
- the `MediaConverter` timeout reads from the live value per call;
- `logging.New` builds the root at `TraceLevel` and applies the level with
  `zerolog.SetGlobalLevel`; `onChange` re-applies it (`WUPHF_LOG_LEVEL`
  still wins);
- Telegram's mode validation (`config.go:106-118`) is expressed in the
  schema (`enum`) **and** kept in `Validate()` for the file.

**Verify**: Go tests in the test plan pass;
`go vet ./... && go test -race ./...` in every Go module → exit 0.

Commit 4.

### Step 5: Document

`docs/connector-protocol.md`: the `settings` registration field, the
live-safe rule, base-vs-override semantics, and that a connector without a
declaration keeps its stored section. Each connector's
`config.example.toml`: mark the runtime keys with a `# Runtime.` comment, as
the assistant bot's example does.

Commit 5.

### Step 6: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
and every Go module's `go vet` + `go test -race` → all exit 0.

## Test plan

Rust (`crates/wuphf-runtime`):

1. `declaration_creates_a_section` — register with a declaration; the
   catalog has `connector_settings.telegram-main.rendering.edit_mode`.
2. `value_failing_the_schema_is_rejected` — `set` `edit_mode = "sideways"` →
   `InvalidValue`, nothing stored.
3. `restart_only_keys_are_refused` — a declaration listing `session_file` →
   registration fails with `INVALID_ARGUMENT`; the hub has no session.
4. `declaration_survives_restart` — store a declaration and an override,
   build a new store from the same storage, the override applies before any
   connector registers.
5. `registration_without_declaration_keeps_the_section`.
6. `core_file_cannot_set_connector_settings` (F2).
7. `unset_returns_to_the_connector_base`.
8. `watch_reports_connector_section_changes` — a `Watch` subscriber filtered
   to the connector's path sees the change.
9. `registration_equality_ignores_the_declaration` — the hub still matches
   the configured entry when a declaration is attached.

Storage: the parity case and the `migration_tests.rs` case from Step 2.

Go (`connectors/shared/settings`):

10. `TestDeclareListsOnlyTaggedFields` and `TestDeclareSchemaUsesTomlNames`.
11. `TestLiveSwapIsAtomic` (run with `-race`, concurrent `Load`/store).
12. `TestWatchAppliesPushedValues` against a fake botconf client.

Go (connectors):

13. Telegram `TestEditModeChangeAppliesToNextDispatch` — swap
    `Rendering.EditMode` to `repost` between two edits; the second posts a
    reply (reuse the recording invoker from `sticker_label_test.go`).
14. WhatsApp `TestStickerMetadataChangeAppliesToNextSticker`.
15. `TestLogLevelChangeIsApplied` in `connectors/shared/logging`.

## Done criteria

- [ ] All Rust and Go tests above exist and pass; clippy, fmt, vet clean
- [ ] `git status --short proto/go` showed only `common.pb.go` before commit 1
- [ ] Every connector's existing `config.example.toml` still loads unchanged
- [ ] `plans/README.md` row updated; F1–F7 recorded in the dependency notes

## STOP conditions

- The tagged botconf has no dynamic sections (see Prerequisite).
- Both `jsonschema` and `boon` fail to build on rustc 1.95, or carry a
  non-MIT/Apache license.
- Keeping the connectors' TOML layout unchanged is impossible with the
  `Runtime` split.
- Regenerating Go bindings changes any file other than `common.pb.go`.
- Applying a pushed value needs a lock on the send path (it must be a single
  atomic load per dispatch).

## Maintenance notes

- `whatsapp-rust` (F6): when picked up, mirror the Go helper with an
  `arc_swap::ArcSwap<Runtime>` and `schemars` for the schema.
- Restart-required runtime keys (Q14 option B) would add a `restart` flag to
  botconf's `RuntimeSetting` and a "pending restart" marker in the panel.
- Instagram's `backfill_on_start` is read once at connect; it could become
  live if the connector re-reads it on each reconnect.
