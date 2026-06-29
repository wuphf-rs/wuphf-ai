# Plan 016: Design spike — a lifecycle policy for delivered media blobs

> **Executor instructions**: This is a DESIGN SPIKE, not a build plan. The
> deliverable is a design document with measurements and a recommended policy
> — code changes are limited to read-only measurement queries. Follow the
> steps; if a STOP condition occurs, stop and report. When done, update the
> status row in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-runtime/src/media.rs crates/wuphf-storage/src/sqlite.rs migrations/`
> On mismatch with the excerpts below, re-verify before designing.

## Status

- **Priority**: P2 (direction)
- **Effort**: S-M for the spike; implementation estimated M
- **Risk**: LOW (spike is read/measure-only)
- **Depends on**: none
- **Category**: direction
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

Media bytes live as BLOBs inside the working SQLite database, and **only
unreferenced (orphan) uploads are ever pruned** — any blob linked to a message
is retained forever (`docs/architecture.md`: "linked media is retained"). The
consequence is already visible: the operator's `wuphf.sqlite` is **166 MB
after light personal use**. The roadmap itself names this gap:
"define a media-deletion policy" (`docs/architecture.md:110`). Without a
policy the database grows monotonically, every backup copies all historical
media, and SQLite page cache efficiency degrades. The spike chooses the
policy; it deliberately does not presume blobs must leave SQLite.

## Current state

- Storage: `media_blobs(id, metadata, contents BLOB, stored_at_ms)`
  (`migrations/0002_media_blobs.sql` + `0003_media_retention.sql` adds
  `stored_at_ms`); written by `put_media`
  (`crates/wuphf-storage/src/sqlite.rs:368-378`), read whole by `media()`
  (sqlite.rs:385-397).
- Reference tracking already exists: `message_media(message_id, media_id)`
  junction (visible at sqlite.rs:276-281, maintained by `put_message`), and
  pruning uses it — `prune_unreferenced_media_before(cutoff_ms)` deletes
  blobs with no `message_media` row older than the cutoff (sqlite.rs:399-405
  region).
- Pruning is triggered **only during uploads** — `GrpcMediaService` calls it
  inline in `upload_media` (`crates/wuphf-runtime/src/media.rs:86-93`) with
  `orphan_media_retention_ms` (default 24h, `crates/wuphf-cli/src/main.rs:50-52`).
  No periodic task: an idle server never prunes anything.
- Delivery model: the destination connector downloads blob bytes by media id
  while executing a dispatch (`docs/architecture.md` Media section). After
  every target has acknowledged, the bytes' only remaining consumers are
  (a) redelivery of an unacked dispatch — impossible once all are
  acknowledged — and (b) **reply context**: `ReplyContext.attachments` carries
  `Attachment` metadata only, not bytes (`crates/wuphf-model/src/message.rs:76-84`),
  so quoting a parent does NOT re-download media. Verify (b) holds for all
  connectors during the spike.
- Dispatch states that gate "fully delivered":
  `DispatchState::{Pending,Blocked,Acknowledged,Failed}`
  (`crates/wuphf-model/src/dispatch.rs:106-118`); `dispatches.message_id`
  column exists (nullable since `migrations/0007`).

## Commands you will need (read-only measurement)

| Purpose | Command | Expected |
|---------|---------|----------|
| Blob footprint | `sqlite3 wuphf.sqlite "SELECT count(*), sum(length(contents))/1024/1024 FROM media_blobs;"` | count + MB figure for the doc |
| Deliverable share | `sqlite3 wuphf.sqlite "SELECT count(DISTINCT mm.media_id) FROM message_media mm WHERE NOT EXISTS (SELECT 1 FROM dispatches d WHERE d.message_id = mm.message_id AND d.state IN ('pending','blocked'));"` | how many blobs are past their last required read |
| DB total vs blob share | `ls -l wuphf.sqlite` vs the sum above | what % a policy could reclaim |

(Run against a **copy** of the database if the server is live.)

## Scope

**In scope**: design document `plans/016-media-lifecycle-design.md` with
measurements, evaluated options, and one recommendation + implementation
outline.

**Out of scope**: any code/schema change; deleting anything from the
operator's database.

## Steps

### Step 1: Measure

Run the queries above on a copy of `wuphf.sqlite`; record blob count, total
MB, share already fully delivered, and growth rate if estimable (blob count
vs `stored_at_ms` range).

### Step 2: Enumerate and evaluate options

Evaluate at least these, each with: reclaimable space, failure modes, schema
changes, and interaction with the invariant that copies stay resolvable
(edits/deletes/replies must keep working after media is gone — they need ids
and metadata, not bytes):

1. **Delivered-media retention window** (recommended starting point): drop
   `contents` (keep the `media_blobs` row + metadata) once every dispatch of
   every message referencing the blob is `Acknowledged`/`Failed` AND
   `stored_at_ms` is older than a configurable retention (e.g. 7 days).
   Late consumers get a clean `MediaNotFound`. Requires: a "contents purged"
   representation (NULL contents or moved row), a periodic prune task
   (the upload-inline trigger is insufficient — fix that here too), and
   `VACUUM`/`auto_vacuum` consideration since SQLite does not return space
   to the OS otherwise. INCREMENTAL auto_vacuum vs scheduled `VACUUM INTO`
   — measure on the copied DB and record numbers.
2. **Filesystem blob store** (bytes on disk under a data dir, DB keeps
   metadata + path/hash): better for large media and backups, but introduces
   partial-failure consistency (file written, row not, and vice versa),
   backup atomicity loss, and a migration for 166MB of existing blobs.
3. **Do nothing but document** ("media kept forever is a feature for a
   personal archive"): valid verdict — record what growth rate makes it
   untenable.

### Step 3: Recommend and outline

Pick one option; write the implementation outline (schema migration, storage
trait additions, the periodic task's home — likely a `tokio::time::interval`
task spawned in `serve()` next to the runtime task,
`crates/wuphf-cli/src/main.rs:124-127` — config knobs and defaults), the test
list, and rollback story. List open questions with recommended answers
(e.g. should `deleted` messages' media purge immediately?).

## Done criteria

- [ ] `plans/016-media-lifecycle-design.md` exists with real measurements from the operator DB copy
- [ ] Each option has concrete reclaim/risk numbers or "not measurable because X"
- [ ] One recommendation with an implementation outline and config surface
- [ ] Reply-context assumption (metadata-only) verified against all three connectors' reply rendering, with file:line cites
- [ ] No source/database modification (`git status` clean apart from plans/)
- [ ] `plans/README.md` status row updated

## STOP conditions

- A connector turns out to re-download parent media bytes when rendering a
  reply quote (breaks the central assumption — the retention design must then
  track reply reachability; report first).
- `message_media` is not actually maintained on every message write (check
  `put_message`) — reference counting would be unsound.

## Maintenance notes

- Whichever option lands must also revisit the **prune trigger**: inline-on-
  upload only (today's behavior) means an idle server never reclaims space
  even for orphans. The periodic task from Step 3 should own both prunes.
- Plan 006 (metadata-only validation) reduces ingest reads of blob contents;
  it is independent but makes option 1's "contents purged" state safer
  (ingest never touches contents).
