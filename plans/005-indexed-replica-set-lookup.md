# Plan 005: Replace the full-table scan in replica_set_for_endpoint with an indexed lookup

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-storage/src/sqlite.rs migrations/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S-M
- **Risk**: LOW (behavior preserved; migration backfill is the only risky bit)
- **Depends on**: plans/004-storage-backend-parity-tests.md (parity suite is the safety net)
- **Category**: perf
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

`replica_set_for_endpoint` is called once per inbound event on the ingest hot
path (`crates/wuphf-core/src/lib.rs:63-67`). The SQLite implementation fetches
**every row** of `replica_sets` and JSON-deserializes each payload to scan for
the endpoint. With a `source`-rule deployment that mints one replica set per
source chat, this is O(all conversations) CPU + allocation per message,
growing without bound as chats accumulate. A junction table keyed by
endpoint id makes it one indexed point query.

## Current state

- `crates/wuphf-storage/src/sqlite.rs:69-88` — the scan:

```rust
    async fn replica_set_for_endpoint(
        &self,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<ReplicaSetId>> {
        let needle = endpoint_id.to_string();
        let rows = sqlx::query("SELECT payload FROM replica_sets")
            .fetch_all(&self.pool)
            .await?;
        for row in rows {
            let replica_set: ReplicaSet = serde_json::from_str(row.get("payload"))?;
            if replica_set
                .endpoints
                .iter()
                .any(|id| id.to_string() == needle)
            {
                return Ok(Some(replica_set.id));
            }
        }
        Ok(None)
    }
```

- `crates/wuphf-storage/src/sqlite.rs:52-60` — the writer that must maintain
  the new junction table:

```rust
    async fn put_replica_set(&self, replica_set: ReplicaSet) -> StorageResult<()> {
        upsert_json(
            &self.pool,
            "replica_sets",
            "id",
            replica_set.id.to_string(),
            &replica_set,
        )
        .await
    }
```

- Schema: `replica_sets(id TEXT PRIMARY KEY, payload TEXT)` — created as
  `replication_groups` in `migrations/0001_initial.sql`, renamed by
  `migrations/0006_rename_replication_group.sql`. Payload JSON shape (from
  `ReplicaSet` in `crates/wuphf-model/src/routing.rs`): `{"id": "...",
  "endpoints": ["...", "..."]}`.
- Migrations: sequential files `migrations/000N_*.sql`, applied by sqlx
  (`sqlx 0.8` with `migrate` feature). Last existing:
  `0007_nullable_dispatch_message_id.sql`. New file must be `0008_*.sql`.
- `InMemoryStorage`'s linear scan (`crates/wuphf-storage/src/memory.rs:84-96`)
  is fine in-memory — leave it; parity tests (plan 004) pin identical
  semantics.
- Endpoints are only ever added to replica sets via `put_replica_set` (the
  whole set is rewritten); there is no partial endpoint-removal API.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Storage tests | `cargo test -p wuphf-storage` | exit 0, incl. parity tests from plan 004 |
| Full gate | `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` | exit 0 |
| Migration smoke | see Step 3 | backfilled rows match |

## Scope

**In scope** (the only files you should create/modify):
- `migrations/0008_replica_set_endpoints.sql` (create)
- `crates/wuphf-storage/src/sqlite.rs` (`put_replica_set`,
  `replica_set_for_endpoint`)
- `crates/wuphf-storage/src/tests.rs` (only if plan 004's
  `replica_set_for_endpoint` parity test needs an update-path case added)

**Out of scope** (do NOT touch):
- `crates/wuphf-storage/src/memory.rs` — linear scan is fine in-memory.
- The `Storage` trait signature.
- `crates/wuphf-core/` — caller is unchanged.
- Existing migration files — never edit an applied migration.

## Git workflow

- Branch: `advisor/005-indexed-replica-set-lookup`
- Commit style: Conventional Commits; suggested `perf: index replica-set membership for endpoint lookup`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Write the migration

Create `migrations/0008_replica_set_endpoints.sql`:

```sql
CREATE TABLE replica_set_endpoints (
    endpoint_id TEXT PRIMARY KEY NOT NULL,
    replica_set_id TEXT NOT NULL
);

CREATE INDEX replica_set_endpoints_set_idx
    ON replica_set_endpoints (replica_set_id);

INSERT INTO replica_set_endpoints (endpoint_id, replica_set_id)
SELECT json_each.value, replica_sets.id
FROM replica_sets, json_each(replica_sets.payload, '$.endpoints');
```

`endpoint_id` is the primary key because an endpoint belongs to at most one
replica set (the `Storage` trait returns `Option<ReplicaSetId>`, and
`wuphf-core` mints one set per conversation).

**Verify**: `cargo test -p wuphf-storage` → migrations apply (every sqlite
test connects fresh and runs all migrations); exit 0.

### Step 2: Maintain the junction table in `put_replica_set`

Rewrite `put_replica_set` (sqlite.rs:52-60) to run in one transaction:

1. the existing `upsert_json` into `replica_sets` (inline its statement or
   call the helper with the transaction — note `upsert_json` takes a pool;
   simplest is to write the upsert inline against `&mut *transaction`,
   mirroring the SQL in `upsert_json` at sqlite.rs:586-589),
2. `DELETE FROM replica_set_endpoints WHERE replica_set_id = ?`,
3. one `INSERT INTO replica_set_endpoints (endpoint_id, replica_set_id) VALUES (?, ?)`
   per `replica_set.endpoints` entry,
4. commit.

Follow the existing transaction pattern in `put_alias`
(sqlite.rs:90-, `self.pool.begin()` … `transaction.commit().await?`).

**Verify**: `cargo test -p wuphf-storage` → exit 0.

### Step 3: Rewrite the lookup

Replace the body of `replica_set_for_endpoint` with:

```rust
let row = sqlx::query(
    "SELECT replica_set_id FROM replica_set_endpoints WHERE endpoint_id = ?",
)
.bind(endpoint_id.to_string())
.fetch_optional(&self.pool)
.await?;
```

mapping the row to `ReplicaSetId` the way other id columns are parsed in this
file (search for how `StorageError::Identifier` is used on malformed stored
ids and match that conversion).

Migration smoke-check against real data (read-only): if a production database
exists at the repo root (`wuphf.sqlite`), **copy it** to a temp path first,
then compare old/new semantics:

```sh
cp wuphf.sqlite /tmp/wuphf-mig-test.sqlite
sqlite3 /tmp/wuphf-mig-test.sqlite "CREATE TABLE replica_set_endpoints (endpoint_id TEXT PRIMARY KEY NOT NULL, replica_set_id TEXT NOT NULL); INSERT INTO replica_set_endpoints SELECT json_each.value, replica_sets.id FROM replica_sets, json_each(replica_sets.payload, '\$.endpoints'); SELECT (SELECT count(*) FROM replica_set_endpoints), (SELECT count(json_each.value) FROM replica_sets, json_each(replica_sets.payload, '\$.endpoints'));"
rm /tmp/wuphf-mig-test.sqlite
```

**Verify**: the two counts printed are equal; `cargo test -p wuphf-storage` →
exit 0, including the plan-004 parity test
`replica_set_for_endpoint` on both backends.

## Test plan

- Plan 004's parity test already covers found/not-found on both backends.
- Add one generic case (both backends): `put_replica_set` called **twice** for
  the same set id with different endpoint lists → lookup by a removed endpoint
  returns `None`, lookup by an added endpoint returns the set (pins the
  DELETE-then-INSERT in Step 2).
- Gate: `cargo test --workspace` → exit 0.

## Done criteria

- [ ] `grep -n 'SELECT payload FROM replica_sets' crates/wuphf-storage/src/sqlite.rs` → no match in `replica_set_for_endpoint`
- [ ] `migrations/0008_replica_set_endpoints.sql` exists with table + index + backfill
- [ ] `cargo test --workspace` exit 0; parity tests pass on both backends
- [ ] Migration smoke-check counts equal (when `wuphf.sqlite` present)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Plan 004's parity suite is absent (`grep InMemoryStorage
  crates/wuphf-storage/src/tests.rs` → no matches) — land plan 004 first.
- You find a code path where an endpoint legitimately belongs to **two**
  replica sets (the PRIMARY KEY on `endpoint_id` would reject it).
- The backfill `json_each` query errors on the bundled SQLite (JSON1 missing)
  — report; an application-side backfill would be needed instead.
- `migrations/0008_*.sql` already exists with different content.

## Maintenance notes

- Any future API that removes a single endpoint from a set must also delete its
  `replica_set_endpoints` row — keep all writes inside `put_replica_set` if
  possible.
- Reviewer should scrutinize the transaction in Step 2 (the junction table must
  never be out of sync with the payload JSON — that is the new invariant).
- Deferred: the same full-scan-over-JSON pattern exists in other `get_json`
  consumers but none are on the per-message hot path; not worth doing now.
