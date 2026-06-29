# Plan 010: Storage hardening trio — blocked-release ordering, poisoned locks, SQL helper allowlist

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-storage/src/sqlite.rs crates/wuphf-core/src/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. **Plan 009 rewrites the same
> `lock().unwrap()` sites — run this plan only after 009 is DONE or REJECTED,
> and re-locate the lock sites first.**

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/009-durable-provisioning-stash.md (same code regions; avoid conflicts)
- **Category**: bug / tech-debt
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

Three small, real-but-low-impact issues, batched because each is under an hour:

1. **Blocked-dispatch release order is unspecified.** Releasing replies blocked
   on the same parent uses a `SELECT` with no `ORDER BY`; SQLite happens to
   return rowid order today, but that is an implementation detail. Two replies
   to one parent can be released — and therefore delivered — out of arrival
   order.
2. **A panic while holding the provisioning mutex poisons it forever.** All
   `self.provisioning.lock().unwrap()` sites then panic on every subsequent
   message: one bad message turns into a permanently wedged core until
   restart.
3. **`upsert_json`/`get_json` build SQL with `format!`.** All call sites pass
   literal table/column names today, so this is not an injection bug — but it
   is an injection *pattern* guarded only by convention.

## Current state

- No ORDER BY on the blocked-release query,
  `crates/wuphf-storage/src/sqlite.rs:490-492`:

```rust
        let rows = sqlx::query(
            "SELECT payload FROM dispatches WHERE state = 'blocked' AND waiting_for = ? AND target_endpoint = ?",
        )
```

  (Contrast: the pending redelivery query at sqlite.rs:430 already orders —
  `"SELECT payload FROM dispatches WHERE state = 'pending' ORDER BY rowid"`.)

- Lock sites (re-locate after plan 009; at `15aa85f` they are):
  `crates/wuphf-core/src/ingest.rs:95`, `ingest.rs:204`,
  `crates/wuphf-core/src/acknowledge.rs:65` — all
  `self.provisioning.lock().unwrap()`, none held across `.await`.

- The format!-built helpers, `crates/wuphf-storage/src/sqlite.rs:579-604`:

```rust
async fn upsert_json<T: serde::Serialize>(
    pool: &SqlitePool,
    table: &str,
    key_column: &str,
    ...
    let sql = format!(
        "INSERT INTO {table} ({key_column}, payload) VALUES (?, ?)
         ON CONFLICT({key_column}) DO UPDATE SET payload = excluded.payload"
    );
...
    let sql = format!("SELECT payload FROM {table} WHERE {key_column} = ?");
```

  Callers (all in sqlite.rs, all literals): `put_endpoint`, `endpoint`,
  `put_replica_set`, `replica_set`, `put_contact`, `put_chat_settings`,
  `put_conversation`, etc. — enumerate with `grep -n 'upsert_json\|get_json' crates/wuphf-storage/src/sqlite.rs`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| Full gate | `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` | exit 0 |

## Scope

**In scope**:
- `crates/wuphf-storage/src/sqlite.rs` (ORDER BY; helper signature)
- `crates/wuphf-core/src/ingest.rs`, `acknowledge.rs` (lock recovery)
- `crates/wuphf-storage/src/tests.rs` (ordering test)

**Out of scope**:
- Switching to `parking_lot` (new dependency for a one-pattern fix — rejected).
- Any schema change.
- `InMemoryStorage` release ordering — verify it preserves insertion order
  (Vec-backed) and only touch it if the new parity test fails.

## Git workflow

- Branch: `advisor/010-storage-hardening`
- One commit per item; Conventional Commits (`fix: release blocked dispatches in arrival order`, `fix: recover poisoned provisioning lock`, `refactor: allowlist json table names`).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: ORDER BY on blocked release

Append ` ORDER BY rowid` to the query at sqlite.rs:490-492. Add a parity test
(plan 004 harness): enqueue two dispatches blocked on the same parent at the
same endpoint, acknowledge the parent, assert release order matches enqueue
order on **both** backends.

**Verify**: `cargo test -p wuphf-storage` → exit 0 incl. the new test.

### Step 2: Poisoned-lock recovery

At each `self.provisioning.lock().unwrap()` site, replace with:

```rust
let mut provisioning = self
    .provisioning
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
```

(If plan 009 consolidated lock access into a helper, change the helper once.)

**Verify**: `grep -n 'provisioning.lock().unwrap()' crates/wuphf-core/src -r`
→ no matches; `cargo test -p wuphf-core` → exit 0.

### Step 3: Allowlist the JSON helper tables

Change `upsert_json`/`get_json` to take a `JsonTable` enum instead of
`&str` pairs:

```rust
#[derive(Clone, Copy)]
enum JsonTable {
    Endpoints,
    ReplicaSets,
    Conversations,
    Contacts,
    ChatSettings,
    // ...one variant per existing caller; derive from the grep
}

impl JsonTable {
    fn table(self) -> &'static str { ... }
    fn key_column(self) -> &'static str { ... }
}
```

Update all callers mechanically. No behavior change; `format!` now consumes
only `&'static str` values originating from the enum.

**Verify**: `cargo test --workspace && cargo clippy --workspace --all-targets`
→ exit 0.

## Test plan

- New parity test for release ordering (Step 1).
- Steps 2-3 are behavior-preserving; existing suites are the gate.

## Done criteria

- [ ] Blocked-release query contains `ORDER BY rowid`; ordering parity test passes on both backends
- [ ] No `provisioning.lock().unwrap()` remains under `crates/wuphf-core/`
- [ ] `upsert_json`/`get_json` no longer accept `&str` table/column names
- [ ] `cargo test --workspace` exit 0
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Plan 009's diff has materially restructured `ingest.rs`/`acknowledge.rs`
  and the lock sites are unrecognizable against this plan's excerpts.
- The ordering parity test fails on `InMemoryStorage` (real divergence —
  report, fix belongs to a reviewed change).
- Any `upsert_json`/`get_json` caller passes a non-literal table name
  (assumption broken — that would be an actual injection risk; report it).

## Maintenance notes

- The `JsonTable` enum is the place to add future JSON-payload tables; never
  reintroduce a `&str` table parameter.
- The poisoned-lock recovery logs nothing by design (the panic that poisoned
  it already surfaced); if panics are observed in provisioning code, fix the
  panic, not the recovery.
