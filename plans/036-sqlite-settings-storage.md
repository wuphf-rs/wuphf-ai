# Plan 036: Keep runtime settings overrides in SQLite

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-storage migrations crates/wuphf-settings crates/wuphf-cli/src/main.rs`
> Plan 035 must have landed (`crates/wuphf-settings` exists and
> `wuphf_settings::load` takes a `botconf::Storage`); if not, STOP. The next
> free migration number must still be `0015` (`ls migrations/`); if another
> plan took it, use the next free number everywhere this plan says `0015` —
> that alone is not a STOP.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW — a new table and a new role trait; nothing existing reads or
  writes them
- **Depends on**: 035
- **Category**: direction (runtime settings, step 2 of wuphf's part)
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

botconf keeps runtime overrides in a `botconf::Storage`. Plan 035 started the
store with botconf's in-memory storage, so an override would vanish on
restart. botconf ships a SeaORM storage, but wuphf persists through sqlx
behind its own `wuphf-storage` role traits, and AGENTS.md requires persistence
to stay behind those traits with schema changes as migrations. This plan adds
a settings table, a role trait both backends implement, and a thin adapter
from that trait to `botconf::Storage`. After it, a stored override survives a
restart (there are still no runtime keys to override — plan 037 adds them).

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Overrides live in wuphf's own SQLite database, created by a migration in `migrations/`, read and written only through a `wuphf-storage` role trait. |
| 2 | `wuphf-storage` does not depend on botconf. The adapter to `botconf::Storage` lives in `wuphf-settings`. |
| 3 | Semantics match botconf's `SeaOrmStorage`: one row per dotted key holding JSON text; a write deletes some keys and upserts at most one, **atomically**; each row records who changed it and when. |
| 4 | The new role trait is **not** part of the `Storage` sum trait: the core never reads settings, so `Core<S: Storage>` must not require it. Both backends implement it anyway (dual-backend parity). |

### Decisions this plan makes (confirmed by the owner 2026-10-25)

- **`updated_by` is `TEXT`, not an integer.** botconf's `StoredOverride.by` is
  an `Option<i64>` (a Telegram user id in the assistant bot). wuphf's actors
  are platform user ids on a connector (`telegram:user:704338780`, a WhatsApp
  JID), which are not integers. The column is text so plans 039–041 can record
  the real actor; the adapter converts botconf's `i64` to its decimal string
  until botconf's `by` becomes a string (raise this in the botconf plans: the
  `by` field should be `Option<String>`).
- **Times are integer milliseconds** (`updated_at_ms`), like every other wuphf
  table (`stored_at_ms`, observation timestamps), not SeaORM's timestamp type.

## Current state

- `crates/wuphf-storage/src/lib.rs:1-18` — crate doc and imports; role traits:
  `TopologyStore` (line 47), `MessageStore` (109), `MediaStore` (137),
  `DispatchStore` (145), `ProvisioningStash` (165), `ObservationStore` (190);
  the sum `pub trait Storage: TopologyStore + MessageStore + MediaStore +
  DispatchStore + ProvisioningStash + ObservationStore {}` with a blanket impl
  (lines 248-264). `StorageError` (line 20-40) wraps `sqlx::Error`,
  `MigrateError`, `serde_json::Error`. All traits use `#[async_trait]`.
- `crates/wuphf-storage/src/sqlite.rs:22` —
  `pub(crate) static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");`
  `SqliteStorage::connect` (line 29-38) runs it; the pool has
  `max_connections(1)`. Example of an upsert with an injected time
  (`MediaStore::put_media`, sqlite.rs:382-399):

  ```rust
  sqlx::query(
      "INSERT INTO media_blobs (id, metadata, contents, stored_at_ms) VALUES (?, ?, ?, ?)
       ON CONFLICT(id) DO UPDATE SET
         metadata = excluded.metadata, contents = excluded.contents,
         stored_at_ms = excluded.stored_at_ms",
  )
  ```

- `crates/wuphf-storage/src/memory.rs:19-40` — `InMemoryStorage { state:
  RwLock<State> }` with one `State` struct holding a field per table; trait
  impls lock `self.state.write().await` / `.read().await`.
- `migrations/` (repo root) — `0001_initial.sql` … `0014_merge_conversations.sql`;
  the next free number is `0015`. `0009_provisioning_stash.sql` is a model for
  a small table with a comment block explaining it.
- `crates/wuphf-storage/src/tests.rs` — dual-backend pattern: one generic
  `async fn scenario<S: Storage>(storage: S)` plus a `memory_*` and a
  `sqlite_*` `#[tokio::test]` calling it; the sqlite variant opens a temp file
  (`std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()))`)
  and can reopen it to prove persistence (lines 7-27).
- `crates/wuphf-storage/src/migration_tests.rs` — for **data** migrations that
  rewrite stored JSON (`database_before(version)` helper). This plan's
  migration only creates a table, so it needs no case there.
- botconf's storage contract (at the pinned tag, `botconf::storage`):

  ```rust
  pub type StorageError = Box<dyn std::error::Error + Send + Sync>;
  pub struct StoredOverride { pub key: String, pub value: String, pub by: Option<i64> }
  pub trait Storage: Send + Sync + 'static {
      fn load(&self) -> BoxFuture<'_, Result<Vec<StoredOverride>, StorageError>>;
      fn write<'a>(&'a self, deleted: &'a [String], upserted: Option<&'a StoredOverride>)
          -> BoxFuture<'a, Result<(), StorageError>>;
  }
  ```

  Its `SeaOrmStorage` deletes `deleted` and upserts `upserted` in one
  transaction, ordered by key on load.
- `crates/wuphf-settings` (from 035) — `load(schema, base, storage)`; `serve()`
  in `crates/wuphf-cli/src/main.rs` passes `botconf::MemoryStorage::new()` and
  connects `SqliteStorage` separately for the core.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| Settings tests | `cargo test -p wuphf-settings` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline + new tests |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| Boundary | `cargo tree -p wuphf-storage --depth 1 \| grep botconf` | no output |

## Scope

**In scope**: `migrations/0015_settings.sql` (create);
`crates/wuphf-storage/src/{lib.rs,sqlite.rs,memory.rs,tests.rs}`;
`crates/wuphf-settings/src/{lib.rs,storage.rs (create),tests.rs}`, its
`Cargo.toml`; `crates/wuphf-cli/src/main.rs`; `docs/architecture.md`
(one sentence).

**Out of scope**: `wuphf-core`; the `Storage` sum trait's membership; any
runtime key (plan 037); botconf itself.

## Git workflow

- Commit on the current branch. Do not create a branch, push, or open a PR
  unless the operator instructed it.
- Commits:
  1. `feat(storage): store runtime settings overrides`
  2. `feat(settings): persist overrides in the core database`

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ record it.

### Step 1: The migration

`migrations/0015_settings.sql`:

```sql
-- Runtime settings overrides (botconf): configuration keys changed at runtime,
-- layered over the config file and the environment. One row per dotted key;
-- `value` is the JSON-encoded value. Written only through the settings store,
-- which validates every change against the whole configuration first.
-- `updated_by` names who changed it (a platform user, or NULL for the CLI).

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_by TEXT,
    updated_at_ms INTEGER NOT NULL
);
```

**Verify**: `cargo build -p wuphf-storage` → exit 0 (the `migrate!` macro
embeds the file at build time).

### Step 2: The role trait and both backends

In `crates/wuphf-storage/src/lib.rs`, after `ObservationStore`:

```rust
/// One stored runtime settings override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSetting {
    /// The dotted configuration key, e.g. `rules`.
    pub key: String,
    /// The JSON-encoded value.
    pub value: String,
    /// Who changed it, if known (a platform user id on a connector).
    pub updated_by: Option<String>,
}

/// Runtime settings overrides. Not part of [`Storage`]: the core never reads
/// settings; the settings store does, through `wuphf-settings`.
#[async_trait]
pub trait SettingsOverrides: Send + Sync + 'static {
    /// Every stored override, ordered by key.
    async fn settings_overrides(&self) -> StorageResult<Vec<StoredSetting>>;
    /// Delete the overrides of `deleted`, then store `upserted` (replacing any
    /// override of its key), all at once.
    async fn write_settings_overrides(
        &self,
        deleted: &[String],
        upserted: Option<&StoredSetting>,
        updated_at_ms: i64,
    ) -> StorageResult<()>;
}
```

Export `StoredSetting` and `SettingsOverrides`. Do **not** add the trait to
`Storage`'s bounds.

SQLite (`sqlite.rs`): `settings_overrides` is a `SELECT key, value,
updated_by FROM settings ORDER BY key`. `write_settings_overrides` opens
`self.pool.begin()`, deletes each key in `deleted` (one
`DELETE FROM settings WHERE key = ?` per key is fine — the list is tiny; do
not build an `IN (…)` string), upserts with
`INSERT … ON CONFLICT(key) DO UPDATE SET value = excluded.value,
updated_by = excluded.updated_by, updated_at_ms = excluded.updated_at_ms`,
then commits.

Memory (`memory.rs`): add `settings: BTreeMap<String, StoredSetting>` to
`State`; same semantics under one `write()` lock (atomic by construction).

**Verify**: `cargo build -p wuphf-storage` → exit 0.

### Step 3: Storage tests

In `crates/wuphf-storage/src/tests.rs`, one generic scenario run on both
backends:

- `memory_settings_overrides_round_trip` / `sqlite_settings_overrides_round_trip`
  calling `settings_overrides_round_trip<S: SettingsOverrides>(storage)`:
  upsert `b`, upsert `a` (with `updated_by = Some("telegram:user:7")`), upsert
  `b` again with a new value; load returns `[a, b]` in key order with `b`'s new
  value; then `write(&["a", "missing"], None)` leaves only `b`; then
  `write(&["b"], Some(c))` leaves only `c` (delete and upsert in one call).
- `sqlite_settings_overrides_survive_reopen` — write one override, reopen the
  file with `SqliteStorage::connect`, load it back.

**Verify**: `cargo test -p wuphf-storage` → all pass. Temporarily drop the
`DELETE` loop in the SQLite impl and confirm the round-trip test fails, then
restore it.

Commit 1.

### Step 4: The botconf adapter

`crates/wuphf-settings/src/storage.rs` (`//!`: "Adapts wuphf's settings table
to botconf's storage contract."):

```rust
/// botconf storage over any wuphf backend that keeps settings overrides.
pub struct SettingsStorage<S> {
    backend: Arc<S>,
    now_ms: fn() -> i64,
}

impl<S: SettingsOverrides> SettingsStorage<S> {
    pub fn new(backend: Arc<S>) -> Self; // now_ms = system clock in ms
}

impl<S: SettingsOverrides> botconf::Storage for SettingsStorage<S> { … }
```

- `load` maps each `StoredSetting` to `StoredOverride { key, value, by }`,
  with `by = updated_by.and_then(|actor| actor.parse::<i64>().ok())` (a
  non-numeric actor reads back as `None`; botconf only uses `by` when writing).
- `write` maps `upserted.by` to `Some(by.to_string())` and calls
  `write_settings_overrides(deleted, upserted, (self.now_ms)())`.
- Errors box into `botconf::StorageError` (`Box::new(error) as _`).
- Futures: `Box::pin(async move { … })`.

Add `wuphf-storage.workspace = true` and `futures` (workspace dep if present;
otherwise use `std::pin::Pin<Box<dyn Future<…> + Send + '_>>` directly — do
not add `futures` just for the type alias) to `crates/wuphf-settings/Cargo.toml`.

Tests in `crates/wuphf-settings/src/tests.rs`:

- `sqlite_overrides_survive_a_restart` — a temp-file `SqliteStorage`, a store
  loaded over `SettingsStorage::new(Arc::clone(&sqlite))`. There are no
  runtime keys yet, so write a raw override through the storage
  (`write(&[], Some(&StoredOverride { key: "rules".into(), value: "[]".into(), by: Some(1) }))`)
  and load a second store over a reopened database: the override shows up in
  `store.current().ignored()` (unknown key — ignored with a reason, not
  dropped). This proves the round trip through botconf without depending on
  plan 037's keys. Note in a comment that plan 037 replaces this with a real
  key.
- `adapter_stores_the_actor_as_text` — write with `by: Some(42)`; the backend
  row's `updated_by` is `Some("42")`.

**Verify**: `cargo test -p wuphf-settings` → all pass.

### Step 5: Use it in `serve()`

The settings store now needs the database, so the order becomes:

```rust
let base = wuphf_settings::base(&path)?;
let file: Configuration = base.extract().context("invalid configuration")?;
let storage = Arc::new(SqliteStorage::connect(&file.database_url).await?);
let settings = Arc::new(
    wuphf_settings::load(WuphfSchema::new(), base, SettingsStorage::new(Arc::clone(&storage)))
        .await
        .context("invalid configuration")?,
);
```

`database_url` is read from the file and environment only — it can never be a
runtime key (it is needed to reach the overrides). Everything after uses
`settings.current()` as in 035, and the same `storage` for the core.

`docs/architecture.md`: one sentence where storage is described — "The same
database holds runtime settings overrides (`settings` table), read through
`wuphf-settings`."

**Verify**: `cargo test --workspace` → all pass; start the server with the
example config → starts; `sqlite3 wuphf.sqlite ".schema settings"` on a copy
of the dev database shows the table.

Commit 2.

### Step 6: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0; `cargo tree -p wuphf-storage --depth 1 | grep botconf` → no output.

## Test plan

- `wuphf-storage`: `memory_settings_overrides_round_trip`,
  `sqlite_settings_overrides_round_trip`,
  `sqlite_settings_overrides_survive_reopen`.
- `wuphf-settings`: `sqlite_overrides_survive_a_restart`,
  `adapter_stores_the_actor_as_text`.

## Done criteria

- [ ] `cargo test --workspace` exits 0; the tests above exist by name
- [ ] clippy and fmt clean
- [ ] `Storage`'s bounds are unchanged (`git diff` of the `pub trait Storage:`
      lines is empty)
- [ ] `wuphf-storage` does not depend on botconf
- [ ] `git diff --stat 643829b..HEAD` lists only in-scope paths for this plan
- [ ] `plans/README.md` status row updated

## STOP conditions

- botconf's `Storage` trait or `StoredOverride` differs from "Current state".
- A migration other than `0015_settings.sql` is needed, or the migration
  checksum of an existing migration changes.
- The memory backend cannot be made atomic without changing `State`'s locking
  model.

## Maintenance notes

- When botconf's `StoredOverride.by` becomes a string (recommended in the
  botconf plans), drop the `i64` conversions in the adapter; the column needs
  no change.
- The pre-deploy check for any later data migration touching `settings` runs
  against a copy of the local `./wuphf.sqlite` (there is no production
  database).
