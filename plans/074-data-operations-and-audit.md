# Plan 074: The core's data operations (behind Storage), the audit log, and their notices

> **Executor instructions**: as plan 058. Never build on battery. The audit
> log is a migration with a `migration_tests` case.

## Status

- **Priority**: P1. The owner asked for a database panel and grilled it on
  2026-11-24.
- **Effort**: L
- **Risk**: MED–HIGH. These operations delete and re-point rows, and they
  are permanent (there is no undo, by the owner's choice). Every one needs
  parity tests on both backends.
- **Depends on**: none in wuphf. botconf 018 is needed only by 075.
- **Category**: core / storage

## The series

| Plan | What |
|---|---|
| botconf 018 | Data pages (list, detail, confirm, question, choice) and `Viewer.context` |
| 074 | The operations in the core and storage, the audit log, notices |
| 075 | The data panel (a botconf `DataSource`), `/wuphf` in a topic, and `wuphf-server data …` |
| 076 | Pause a chat: the `paused_chats` runtime setting |
| 077 | Merge and split a chat's address forms |

## Design decisions (settled with the owner, 2026-11-24)

1. **Writes are curated operations only,** each through the `Storage` role
   traits and the core's rules. Raw tables are read-only: conversations,
   endpoints, messages and copies, dispatches, and chat profiles. Nothing
   else can be browsed raw.
2. **Owners only** (the connector's `owner_ids`). Every destructive operation
   is confirmed by the front end and is **permanent**.
3. **Each write is audited** in an audit table, viewable, and posts a notice
   to the notice chats naming the actor, e.g. "Alice unlinked Family's topic".
4. **Unlink topic:**
   - the conversation forgets its mirror thread, so the next message
     provisions a fresh one with an info card;
   - the old topic gets a `RenameThread` to "<title> (unlinked)" and a close;
   - the old copies stay mapped, so replies to old messages still resolve.
5. **Relink to a chosen topic:** if another conversation owns the topic, the
   move is refused, and only **Take over** is offered. Take over unlinks the
   owner, as in 4, and points this conversation at the topic. There is no
   sharing and no merge offer here.
6. **Delete history:**
   - whole, or older than N days;
   - the front end asks each time "wuphf's records only" or "records and
     the mirror-side copies" (`Action::Delete` to mirror copies). The
     source's originals are never deleted.
7. **Purge media:**
   - orphans now, or one conversation's media older than N days;
   - the messages stay, marked "media removed".
8. **Forget chat** removes the messages, copies, profile and identities, and
   unlinks its topic as in 4.
9. **Dispatches:** list them pending or dead-lettered, with their reason.
   Retry or drop one, all of a conversation, or all with the same reason.
   Clear dead letters older than N days.
10. **All of these run once,** when asked. There is no retention policy.

## Planner decisions

- **A `DataOps` service in `wuphf-core`** (`data.rs`), one method per
  operation. Each is a transaction where the backend allows (SQLite), and
  each returns an `AuditEntry` plus the dispatches to send (renames, closes,
  deletes, notes). Routing invariants stay in the core.
- **Storage:** role-trait methods added where they belong:
  - `TopologyStore`: conversations listed and paged, with search by title;
    unlink and relink a mirror endpoint; forget a conversation;
  - `MessageStore`: a page of messages, text search within a conversation,
    delete by conversation and age;
  - `DispatchStore`: list by state, retry (`Failed` → `Pending`), drop, by
    id, conversation or reason;
  - `MediaStore`: orphans, and purge by conversation and age;
  - `ChatProfileStore`: forget.

  Memory and SQLite each get parity tests.
- **Migration `0020_audit_log.sql`:** `id`, `at_ms`, `actor`, `operation`,
  `subject` (a conversation or dispatch id), `detail` (JSON). `AuditStore` is
  a new role trait.
- **Closing a topic** is a new `Action::CloseThread`, which Telegram does
  with `channels.editForumTopic(closed)`. It needs a capability flag, so
  connectors without topics never get it.
- **Notices** go through the existing owner-notice path (`notify.rs`), in one
  wording module (`data_notes.rs`).
- If plan 071 lands first, its `poll_votes` is migration 0020 and this one
  becomes 0021. Take the next free number at execution.

## Steps

1. Baseline tests; the migration and `AuditStore`.
2. Storage methods, with parity tests.
3. `DataOps`:
   - unlink, relink and take over;
   - delete history (both reaches) and purge media;
   - forget;
   - the dispatch operations.

   Each audited, each with its notice, each with core tests.
4. `Action::CloseThread`, its proto and the capability; Telegram does it.
5. Docs: architecture (Data operations, Key Invariants: "a data operation
   keeps every remaining copy resolvable").

## STOP conditions

- A conversation's mirror link turns out to be stored in more than one place
  (for example the endpoint row and the stash), so unlinking one leaves the
  other. Map every holder first, and list them in this plan.
- Deleting messages would orphan the copies of replies held elsewhere. Then
  keep the deleted messages as tombstones (`deleted: true`, content dropped)
  instead of removing the rows.
