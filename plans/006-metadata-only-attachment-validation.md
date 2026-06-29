# Plan 006: Stop loading full media blobs during ingest attachment validation

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-storage/src/ crates/wuphf-core/src/ingest.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/004-storage-backend-parity-tests.md
- **Category**: perf
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

For every inbound message, ingest validates each attachment by calling
`storage.media(...)`, which loads the **entire blob** (`SELECT metadata,
contents`) into memory — only to compare the metadata struct and drop the
bytes. A 30 MB video (`max_media_bytes` defaults to 32 MiB,
`crates/wuphf-cli/src/main.rs:46-48`) costs a 30 MB read+alloc per attachment
per message at ingest time, serially. The validation needs only the metadata
column.

## Current state

- `crates/wuphf-core/src/ingest.rs:45-54` — the per-attachment loop:

```rust
        for attachment in &inbound.attachments {
            let media = self
                .storage
                .media(attachment.media_id)
                .await?
                .ok_or(CoreError::UnknownMedia(attachment.media_id))?;
            if media.attachment != *attachment {
                return Err(CoreError::MediaMetadataMismatch(attachment.media_id));
            }
        }
```

- `crates/wuphf-storage/src/sqlite.rs:385-397` — `media()` loads contents:

```rust
    async fn media(&self, media_id: MediaId) -> StorageResult<Option<MediaBlob>> {
        let row = sqlx::query("SELECT metadata, contents FROM media_blobs WHERE id = ?")
```

- The trait (`crates/wuphf-storage/src/lib.rs:118-120`):

```rust
    async fn put_media(&self, media: MediaBlob, stored_at_ms: i64) -> StorageResult<()>;
    async fn media(&self, media_id: MediaId) -> StorageResult<Option<MediaBlob>>;
    async fn prune_unreferenced_media_before(&self, cutoff_ms: i64) -> StorageResult<u64>;
```

- `Attachment` (metadata) vs `MediaBlob` (metadata + bytes):
  `crates/wuphf-model/src/message.rs:21-35`. The `metadata` column already
  stores exactly a serialized `Attachment` (see `put_media`,
  sqlite.rs:368-378: `.bind(serde_json::to_string(&media.attachment)?)`).
- Who actually needs bytes: only the media broker download path
  (`crates/wuphf-runtime/src/media.rs`, `GrpcMediaService`) when a connector
  downloads for sending. Keep `media()` for it.
- `InMemoryStorage` has its own `media()` in
  `crates/wuphf-storage/src/memory.rs` (find with `grep -n 'fn media'`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| Core tests | `cargo test -p wuphf-core` | exit 0 |
| Full gate | `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-storage/src/lib.rs` — add trait method `media_attachment`
- `crates/wuphf-storage/src/sqlite.rs` — implement (SELECT metadata only)
- `crates/wuphf-storage/src/memory.rs` — implement
- `crates/wuphf-storage/src/tests.rs` — parity test
- `crates/wuphf-core/src/ingest.rs:45-54` — switch the loop to it

**Out of scope** (do NOT touch):
- `crates/wuphf-runtime/src/media.rs` — the broker legitimately needs bytes.
- `media()` itself — keep it; other callers depend on full blobs.
- Deferring validation to dispatch time (rejected: ingest-time rejection of
  unknown media is an invariant — `docs/architecture.md`: "Inbound attachments
  are accepted only after their media and metadata match a broker-issued
  record").

## Git workflow

- Branch: `advisor/006-metadata-only-validation`
- Commit style: Conventional Commits; suggested `perf: validate ingest attachments without loading blob contents`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add `media_attachment` to the `Storage` trait

In `crates/wuphf-storage/src/lib.rs`, next to `media()`:

```rust
    /// The stored metadata for a media blob, without loading its contents.
    /// Used by ingest validation, which only compares metadata.
    async fn media_attachment(&self, media_id: MediaId) -> StorageResult<Option<Attachment>>;
```

(`Attachment` needs importing from `wuphf_model` in lib.rs's existing import
list.)

**Verify**: `cargo build -p wuphf-storage` fails listing exactly the two
backends as missing the method — expected mid-step state.

### Step 2: Implement in both backends

`sqlite.rs` — mirror `media()`'s row handling but select only metadata:

```rust
    async fn media_attachment(&self, media_id: MediaId) -> StorageResult<Option<Attachment>> {
        let row = sqlx::query("SELECT metadata FROM media_blobs WHERE id = ?")
            .bind(media_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| Ok(serde_json::from_str(row.get("metadata"))?))
            .transpose()
    }
```

(match the exact row-access idiom used in `media()` at sqlite.rs:385-397 —
copy its `row.get` pattern.)

`memory.rs` — clone only the `attachment` field of the stored `MediaBlob`.

**Verify**: `cargo test -p wuphf-storage` → exit 0.

### Step 3: Switch ingest

In `crates/wuphf-core/src/ingest.rs:45-54` replace `storage.media(...)` with
`storage.media_attachment(...)` and compare directly:

```rust
        for attachment in &inbound.attachments {
            let stored = self
                .storage
                .media_attachment(attachment.media_id)
                .await?
                .ok_or(CoreError::UnknownMedia(attachment.media_id))?;
            if stored != *attachment {
                return Err(CoreError::MediaMetadataMismatch(attachment.media_id));
            }
        }
```

**Verify**: `cargo test -p wuphf-core` → exit 0 (existing ingest tests cover
the UnknownMedia/MediaMetadataMismatch paths — confirm by
`grep -n 'UnknownMedia\|MediaMetadataMismatch' crates/wuphf-core/src/tests.rs`;
if no test exists for these, add one: ingest a message with an unknown
`media_id` → expect `CoreError::UnknownMedia`).

### Step 4: Parity test

In `crates/wuphf-storage/src/tests.rs`, add a generic body (both backends,
plan-004 style): `put_media` then `media_attachment` returns the stored
`Attachment` and `media_attachment(random_id)` returns `None`.

**Verify**: `cargo test --workspace` → exit 0.

## Test plan

- Parity test from Step 4 (×2 backends).
- Ingest-path test from Step 3 if missing.
- Full gate: `cargo fmt --check && cargo clippy --workspace --all-targets &&
  cargo test --workspace` → exit 0.

## Done criteria

- [ ] `grep -n 'storage.media(' crates/wuphf-core/src/ingest.rs` → wait — use `grep -n '\.media(' crates/wuphf-core/src/ingest.rs` → no matches (only `media_attachment`)
- [ ] `grep -n 'SELECT metadata FROM media_blobs' crates/wuphf-storage/src/sqlite.rs` → 1 match
- [ ] `cargo test --workspace` exit 0
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Another caller of `storage.media(...)` outside `wuphf-runtime/src/media.rs`
  exists (`grep -rn '\.media(' crates/ --include='*.rs'`) that also only needs
  metadata — report it; widening scope is the advisor's call.
- The `metadata` column turns out not to round-trip to `Attachment`
  (deserialization error in tests) — schema assumption broken.

## Maintenance notes

- If batch ingest of multi-attachment messages becomes a bottleneck later, the
  next step is a `media_attachments(Vec<MediaId>)` batch method — not needed at
  current scale.
- Reviewer: confirm no call site still pays for blob contents at ingest
  (search `\.media(` under `crates/wuphf-core/`).
