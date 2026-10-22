# Plan 031: Merge ReplicaSet and ThreadMapping into Conversation

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: confirm plans 026–030 are DONE in
> `plans/README.md`, then run
> `git diff --stat 5c38943..HEAD -- crates/wuphf-model/src/routing.rs crates/wuphf-storage crates/wuphf-core/src migrations`.
> Those plans' changes are expected; the excerpts below already reflect them.
> Any other mismatch is a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M–L
- **Risk**: HIGH — rewrites stored topology with a SQL migration; mitigated by a migration test and a pre-deploy check
- **Depends on**: plans/030-single-dispatch-type.md (and transitively 026–029)
- **Category**: tech-debt
- **Planned at**: commit `5c38943`, 2026-10-21

## Why this matters

One linked chat is spread over three records that always come as a set:
a `Conversation` (rule, source chat, title), exactly one `ReplicaSet` (the
endpoints that mirror each other) and, for mirrors, a `ThreadMapping` that
repeats the thread id the mirror endpoint already stores. A reader has to learn
three concepts for one thing, the storage trait carries seven methods to join
them back up, and finding the conversation for an endpoint scans every replica
set's JSON (`replica_set_for_endpoint` in `crates/wuphf-storage/src/sqlite.rs`).

After this plan a `Conversation` lists its own endpoints, the thread id is read
from the endpoint, and an indexed table answers "which conversation is this
endpoint in". Owner messages to chats outside every rule, which today get a
replica set with no conversation, get a conversation with no rule.

## Current state

`crates/wuphf-model/src/routing.rs`:

```rust
pub struct ReplicaSet { pub id: ReplicaSetId, pub endpoints: Vec<EndpointId> }

pub struct Conversation {
    pub id: ConversationId,
    pub rule_id: RuleId,
    pub replica_set_id: ReplicaSetId,
    pub source_connector: ConnectorId,
    pub source_address: ChatAddress,
    #[serde(default)]
    pub title: Option<String>,
}

pub struct ThreadMapping { pub conversation_id: ConversationId, pub connector_id: ConnectorId, pub container: String, pub thread_id: String }
```

`ReplicaSetId` is a UUID newtype in `crates/wuphf-model/src/ids.rs`
(`uuid_id!(ReplicaSetId, …)`). `ChatRef { connector_id, #[serde(flatten)] address: ChatAddress }`
exists since plan 028. `Message.replica_set_id` and `Dispatch.replica_set_id`
(plan 030) hold replica-set ids.

Storage trait methods involved (`crates/wuphf-storage/src/lib.rs`):
`put_replica_set`, `replica_set` (errors with `StorageError::ReplicaSetNotFound`),
`replica_set_for_endpoint`, `put_conversation`, `conversation`,
`conversation_for_source(rule_id, connector_id, address)`,
`conversation_for_replica_set`, `put_thread_mapping`,
`thread_for_conversation(conversation_id, connector_id)`,
`conversation_for_thread` (no caller anywhere), and the stash methods
`stash_provisioning_message(replica_set_id, …)`,
`drain_provisioning_stash(replica_set_id)`, `stashed_replica_sets()`.

SQLite tables (from `migrations/0004_dynamic_routing.sql`,
`0006_rename_replication_group.sql`, `0009_provisioning_stash.sql`):

```sql
replica_sets (id TEXT PRIMARY KEY, payload TEXT)          -- payload {"id":…, "endpoints":[…]}
conversations (id TEXT PRIMARY KEY, rule_id TEXT NOT NULL, replica_set_id TEXT NOT NULL,
               source_connector TEXT NOT NULL, source_chat_id TEXT NOT NULL,
               source_thread_key TEXT NOT NULL, payload TEXT NOT NULL,
               UNIQUE (rule_id, source_connector, source_chat_id, source_thread_key))
thread_mappings (conversation_id, connector_id, container, thread_id,
                 PRIMARY KEY (conversation_id, connector_id), UNIQUE (connector_id, container, thread_id))
provisioning_stash (replica_set_id, message_id, origin_endpoint, provision_title,
                    PRIMARY KEY (replica_set_id, message_id))
```

`thread_key` columns hold `""` for "no thread". The `endpoints.payload` JSON
is `{"id", "connector_id", "platform", "chat_id", "thread_id"}`.

How the core uses them today:
- `crates/wuphf-core/src/lib.rs` `ingest`: `replica_set_for_endpoint(origin)`,
  else `CoreError::OriginWithoutReplicaSet`.
- `crates/wuphf-core/src/resolve.rs`: `ensure_conversation` creates an empty
  replica set and a conversation pointing at it; `ensure_end_endpoint` mints an
  endpoint and appends it with a read-modify-write of the replica set
  (`replica_set(…)`, push, `put_replica_set(…)`); `endpoint_in_replica_set`
  finds a many-to-one destination's endpoint by connector; `still_covered`
  treats an endpoint with no replica set, or a replica set with no
  conversation, as exempt from rule checks.
- `crates/wuphf-core/src/send.rs` `raw_target_endpoint` (the function that
  mints a standalone endpoint for an owner message) creates
  `ReplicaSet { id: ReplicaSetId::new(), endpoints: vec![endpoint.id] }` with
  no conversation.
- `crates/wuphf-core/src/acknowledge.rs` `persist_provisioned_thread`: stamps
  `thread_id` onto the endpoint, writes a canonical alias, and also writes a
  `ThreadMapping`. The endpoint already holds the thread id, so the mapping is
  redundant.
- `crates/wuphf-core/src/router.rs`: `route` and `fan_out_to_copies` read
  `replica_set(message.replica_set_id).endpoints`; `resolve_target` and
  `route_retitle` call `thread_for_conversation`.
- `crates/wuphf-core/src/ingest.rs` `maybe_retitle`: reads
  `conversation_for_replica_set`, compares the source, then
  `put_conversation(conversation.clone())` with the new title.
- The `Core::provisioning` cache is `Mutex<HashMap<ReplicaSetId, ProvisionState>>`.

The SQLite build bundles SQLite with the JSON functions (`json_extract`,
`json_object`, `json_each`): `sqlx-sqlite` 0.8.6 enables its `bundled`
feature by default, and the lock file pins `libsqlite3-sys` 0.30.1.

## Target design

```rust
/// A set of chats that mirror each other: the unit a rule creates for one
/// source chat. A message arriving at any endpoint is copied to all the others.
pub struct Conversation {
    pub id: ConversationId,
    /// The rule that created it. `None` for a chat outside every rule that the
    /// owner sent a message to directly.
    pub rule_id: Option<RuleId>,
    /// The chat the conversation is named after: the rule's source chat, or
    /// the chat the owner sent to.
    pub source: ChatRef,
    pub endpoints: Vec<EndpointId>,
    /// The last source title seen, used to rename mirror threads.
    #[serde(default)]
    pub title: Option<String>,
}
```

Deleted: `ReplicaSet`, `ReplicaSetId`, `ThreadMapping`. Renamed:
`Message.replica_set_id` and `Dispatch.replica_set_id` → `conversation_id`
(each with `#[serde(alias = "replica_set_id")]`).

Storage trait, conversation section, after this plan:

```rust
/// Insert or replace a conversation, including its endpoint list.
async fn put_conversation(&self, conversation: Conversation) -> StorageResult<()>;
async fn conversation(&self, id: ConversationId) -> StorageResult<Option<Conversation>>;
/// The conversation an endpoint belongs to, if any. Indexed.
async fn conversation_for_endpoint(&self, endpoint_id: EndpointId) -> StorageResult<Option<Conversation>>;
async fn conversation_for_source(&self, rule_id: &RuleId, connector_id: &ConnectorId, address: &ChatAddress) -> StorageResult<Option<Conversation>>;
/// Append an endpoint to a conversation. Atomic and idempotent, so two
/// workers adding endpoints cannot overwrite each other.
async fn add_conversation_endpoint(&self, id: ConversationId, endpoint_id: EndpointId) -> StorageResult<()>;
/// Change only the title, leaving the endpoint list untouched.
async fn set_conversation_title(&self, id: ConversationId, title: &str) -> StorageResult<()>;
```

Removed: `put_replica_set`, `replica_set`, `replica_set_for_endpoint`,
`conversation_for_replica_set`, `put_thread_mapping`, `thread_for_conversation`,
`conversation_for_thread`, and `StorageError::ReplicaSetNotFound`. Stash
methods take `conversation_id: ConversationId`; `stashed_replica_sets` becomes
`stashed_conversations`.

Why two narrow writers instead of read-modify-write through
`put_conversation`: today the endpoint list and the title live in different
records, so updating one cannot clobber the other. Once they share a record, a
title update that rewrote a stale endpoint list could drop an endpoint. The
per-connector ingest workers run concurrently, so writes must be narrow.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Storage tests | `cargo test -p wuphf-storage` | exit 0 |
| Core tests | `cargo test -p wuphf-core` | exit 0 |
| All tests | `cargo test --workspace` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| Pre-deploy check | see Step 8 | 0 rows |

## Scope

**In scope**:
- `crates/wuphf-model/src/{ids,routing,message,dispatch,lib}.rs`
- `crates/wuphf-storage/src/{lib,memory,sqlite,legacy,tests}.rs`
- `migrations/0012_merge_conversations.sql` (create)
- `crates/wuphf-core/src/{lib,resolve,ingest,acknowledge,send,router}.rs`, `crates/wuphf-core/src/tests.rs`
- `crates/wuphf-runtime/src/**` only where a renamed field is read
- `docs/architecture.md`, `docs/index.html`, `README.md` (vocabulary only)

**Out of scope** (do NOT touch):
- `crates/wuphf-proto/**`, `proto/go/**`, `connectors/**`: connectors never see
  replica sets or conversations.
- The provisioning stash's *logic* (only its key type and column name change).
- The alias tables and `address_links`.
- Earlier migrations: never edit an applied migration.

## Git workflow

- Branch: `advisor/031-conversation-absorbs-replica-set`.
- Commits: `feat(storage): migration merging replica sets into conversations`
  (Steps 1–2, with the migration test), then
  `refactor: conversations list their own endpoints` (Steps 3–7).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 0: Baseline

`cargo test --workspace 2>&1 | grep "^test result"`; sum the passed counts.

### Step 1: Write the migration

Create `migrations/0012_merge_conversations.sql` (use the next free number if
0012 is taken):

```sql
-- A conversation and its replica set were always one-to-one, and a thread
-- mapping repeated the thread id each mirror endpoint already stores. Merge
-- them: a conversation now lists its own endpoints.
--
-- Each merged conversation takes its replica set's id. Messages, dispatches and
-- the provisioning stash already store replica-set ids, so they keep pointing at
-- the right record and only their JSON key is renamed. The old conversation ids
-- were referenced only by thread_mappings, which is dropped.
--
-- Replica sets created for owner messages to chats outside every rule had no
-- conversation; they become conversations with no rule, named after their one
-- endpoint.

CREATE TABLE conversations_new (
    id TEXT PRIMARY KEY NOT NULL,
    rule_id TEXT,
    source_connector TEXT NOT NULL,
    source_chat_id TEXT NOT NULL,
    source_thread_key TEXT NOT NULL,
    payload TEXT NOT NULL
);

INSERT INTO conversations_new
    (id, rule_id, source_connector, source_chat_id, source_thread_key, payload)
SELECT
    rs.id,
    c.rule_id,
    c.source_connector,
    c.source_chat_id,
    c.source_thread_key,
    json_object(
        'id', rs.id,
        'rule_id', c.rule_id,
        'source', json_object(
            'connector_id', c.source_connector,
            'chat_id', c.source_chat_id,
            'thread_id', NULLIF(c.source_thread_key, '')
        ),
        'endpoints', json(json_extract(rs.payload, '$.endpoints')),
        'title', json_extract(c.payload, '$.title')
    )
FROM conversations AS c
JOIN replica_sets AS rs ON rs.id = c.replica_set_id;

INSERT INTO conversations_new
    (id, rule_id, source_connector, source_chat_id, source_thread_key, payload)
SELECT
    rs.id,
    NULL,
    json_extract(e.payload, '$.connector_id'),
    json_extract(e.payload, '$.chat_id'),
    COALESCE(json_extract(e.payload, '$.thread_id'), ''),
    json_object(
        'id', rs.id,
        'rule_id', NULL,
        'source', json_object(
            'connector_id', json_extract(e.payload, '$.connector_id'),
            'chat_id', json_extract(e.payload, '$.chat_id'),
            'thread_id', json_extract(e.payload, '$.thread_id')
        ),
        'endpoints', json(json_extract(rs.payload, '$.endpoints')),
        'title', NULL
    )
FROM replica_sets AS rs
JOIN endpoints AS e ON e.id = json_extract(rs.payload, '$.endpoints[0]')
WHERE NOT EXISTS (SELECT 1 FROM conversations AS c WHERE c.replica_set_id = rs.id);

DROP TABLE conversations;
ALTER TABLE conversations_new RENAME TO conversations;
CREATE UNIQUE INDEX conversations_source_idx
    ON conversations (rule_id, source_connector, source_chat_id, source_thread_key);

CREATE TABLE conversation_endpoints (
    endpoint_id TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL
);
INSERT OR IGNORE INTO conversation_endpoints (endpoint_id, conversation_id)
SELECT member.value, c.id
FROM conversations AS c, json_each(c.payload, '$.endpoints') AS member;
CREATE INDEX conversation_endpoints_conversation_idx
    ON conversation_endpoints (conversation_id);

DROP TABLE replica_sets;
DROP TABLE thread_mappings;

UPDATE messages SET payload = replace(payload, '"replica_set_id":', '"conversation_id":');
UPDATE dispatches SET payload = replace(payload, '"replica_set_id":', '"conversation_id":');

ALTER TABLE provisioning_stash RENAME COLUMN replica_set_id TO conversation_id;
```

The `replace(…, '"replica_set_id":', …)` pattern, anchored on the quote and
colon so only JSON keys match, is the same technique
`migrations/0006_rename_replication_group.sql` used.

### Step 2: Test the migration before changing any Rust

Add a test to `crates/wuphf-storage/src/tests.rs` named
`merge_conversations_migration_rewrites_topology`. It must not depend on the
new Rust types yet (they arrive in Step 3), so it uses raw SQL only:

1. Open a fresh SQLite pool on a temp file
   (`sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite://…?mode=rwc")`).
2. Create the pre-migration tables with `sqlx::raw_sql(…)`: copy the
   `CREATE TABLE` statements for `endpoints`, `messages`, `dispatches`
   (0007 shape), `replica_sets`, `conversations` (0004 shape with
   `replica_set_id` instead of `group_id`), `thread_mappings` and
   `provisioning_stash` (0009 shape) from the migration files.
3. Insert: two endpoints (WhatsApp chat `wa-1`; Telegram container `tg-1`
   whose payload `thread_id` is `"57"`); replica set R1 with both; a
   conversation C1 (different id from R1) with `rule_id = 'mirror'`, title
   `"Priya"`; a thread mapping for C1; a third endpoint `wa-2` in replica set
   R2 with no conversation (the owner-message case); one message whose payload
   contains `"replica_set_id":"<R1>"`; one dispatch whose payload contains
   `"replica_set_id":"<R1>"`; one stash row for R1.
4. Run `sqlx::raw_sql(include_str!("../../../migrations/0012_merge_conversations.sql")).execute(&pool)`.
5. Assert with SQL:
   - `SELECT count(*) FROM conversations` = 2.
   - The row with `id = R1` has `rule_id = 'mirror'`, and its payload, parsed
     with `serde_json::Value`, has `source.chat_id == "wa-1"`,
     `endpoints == [<wa-1 id>, <tg-1 id>]`, `title == "Priya"`.
   - The row with `id = R2` has `rule_id IS NULL`, `source.chat_id == "wa-2"`,
     `title == null`.
   - `conversation_endpoints` maps all three endpoints to the right ids.
   - Tables `replica_sets` and `thread_mappings` no longer exist
     (`SELECT count(*) FROM sqlite_master WHERE name IN ('replica_sets','thread_mappings')` = 0).
   - The message and dispatch payloads contain `"conversation_id":"<R1>"` and
     no `replica_set_id`.
   - `SELECT conversation_id FROM provisioning_stash` returns R1.

**Verify**: `cargo test -p wuphf-storage merge_conversations_migration` → 1 passed.
(`SqliteStorage::connect` in other tests will also apply 0012 to fresh
databases; those still pass because the empty tables migrate trivially.)

### Step 3: Model

- `ids.rs`: delete `ReplicaSetId`.
- `routing.rs`: delete `ReplicaSet` and `ThreadMapping`; replace
  `Conversation` with the "Target design" struct. Update the crate doc list
  in `lib.rs` (the `[`ReplicaSet`]` bullet): fold it into the
  `[`Conversation`]` bullet as "the chats that mirror each other".
- `message.rs`: `Message.replica_set_id` → `#[serde(alias = "replica_set_id")] pub conversation_id: ConversationId`.
- `dispatch.rs`: `Dispatch.replica_set_id` → `#[serde(alias = "replica_set_id")] pub conversation_id: ConversationId`;
  `Dispatch::new` takes `conversation_id`.
- Update the model's `legacy_json_tests.rs` sample `Message` (from plan 027) to
  set `conversation_id`; its legacy constant still says `replica_set_id`, which
  now also proves the new alias.

**Verify**: `cargo test -p wuphf-model` → exit 0.

### Step 4: Storage

- `lib.rs`: apply the "Target design" trait changes; delete
  `StorageError::ReplicaSetNotFound`.
- `sqlite.rs`:
  - `put_conversation`: in one transaction, upsert the row
    (`ON CONFLICT(id) DO UPDATE SET payload = excluded.payload`; bind
    `rule_id` as `Option<String>`, the `source.*` columns from
    `conversation.source`), then
    `DELETE FROM conversation_endpoints WHERE conversation_id = ?`, then insert
    one row per endpoint.
  - `conversation_for_endpoint`:
    `SELECT c.payload FROM conversation_endpoints ce JOIN conversations c ON c.id = ce.conversation_id WHERE ce.endpoint_id = ?`.
  - `add_conversation_endpoint`: in one transaction, read the payload, push
    the endpoint only if absent, write it back, and
    `INSERT OR IGNORE INTO conversation_endpoints`. If the conversation is
    missing, return a new `StorageError::ConversationNotFound(ConversationId)`
    variant (add it next to `EndpointNotFound`). `set_conversation_title`
    returns the same error for a missing conversation.
  - `set_conversation_title`: in one transaction, read, set `title`, write.
  - `conversation_for_source`: unchanged SQL (the columns are the same).
  - Stash: `replica_set_id` → `conversation_id` in the three stash queries;
    rename `stashed_replica_sets` → `stashed_conversations`.
  - Delete the removed methods.
- `memory.rs`: remove `replica_sets` and `thread_mappings` from `State`;
  implement the new methods over `conversations` (linear scan for
  `conversation_for_endpoint` is fine in memory); key the stash by
  `ConversationId`.
- `legacy.rs` (plan 030): in `LegacyAction`, rename each `replica_set_id`
  field to `conversation_id` with `#[serde(alias = "replica_set_id")]`
  (after migration 0012, old rows carry the `conversation_id` key).

**Verify**: `cargo build -p wuphf-storage` → exit 0.

### Step 5: Core

- `lib.rs`: `ingest` uses
  `conversation_for_endpoint(origin_endpoint)?.ok_or(CoreError::OriginWithoutConversation(origin_endpoint))?`
  (rename the variant from `OriginWithoutReplicaSet`) and passes
  `conversation.id` where it passed `replica_set_id`. The provisioning cache
  becomes `Mutex<HashMap<ConversationId, ProvisionState>>`.
- `resolve.rs`:
  - `ensure_conversation`: create
    `Conversation { id: ConversationId::new(), rule_id: Some(rule_id.clone()), source: ChatRef { connector_id: identity_connector, address: identity_address.clone() }, endpoints: Vec::new(), title: None }`
    with `put_conversation`. No replica set.
  - `ensure_end_endpoint`: after minting, call
    `add_conversation_endpoint(conversation.id, endpoint.id)` instead of the
    replica-set read-modify-write.
  - `endpoint_in_replica_set` → `endpoint_in_conversation(conversation_id, connector)`.
    It must re-read the conversation from storage
    (`self.storage.conversation(conversation_id)`), not use the `&Conversation`
    passed into `ensure_end_endpoint`: that value is a snapshot taken before
    the source endpoint was added in the same `resolve_origin` call.
  - `still_covered`: `conversation_for_endpoint`; `None` → exempt (return the
    endpoint); `rule_id: None` → exempt; `rule_id: Some(id)` whose rule is
    gone from config → not covered (`None`), as today.
- `send.rs`: use `conversation_for_endpoint` (error
  `OriginWithoutConversation`). The standalone path creates
  `Conversation { id: ConversationId::new(), rule_id: None, source: ChatRef { connector_id, address }, endpoints: vec![endpoint.id], title: None }`.
  `fan_out_to_mirrors` takes the provision title from `conversation.title`,
  falling back to the chat id, as today.
- `ingest.rs` `maybe_retitle`: read with `conversation(conversation_id)`;
  compare `conversation.source.connector_id` / `conversation.source.address.chat_id`
  with the origin; write with `set_conversation_title`; then set
  `conversation.title` on the local copy and call `route_retitle`.
- `acknowledge.rs` `persist_provisioned_thread`: delete the `ThreadMapping`
  block and the now-unused `container` variable; keep the endpoint stamp and
  the canonical alias.
- `router.rs`:
  - `route` and `fan_out_to_copies`: endpoints come from
    `self.storage.conversation(message.conversation_id).await?.ok_or(RouterError::ConversationNotFound(message.conversation_id))?.endpoints`
    (add the variant; rename `OriginOutsideReplicaSet` → `OriginOutsideConversation`).
  - `resolve_target(endpoint, conversation: &Conversation, title)`: a mirror
    endpoint is one where `conversation.rule_id` names a configured rule and
    `is_mirror_side(rule, &endpoint.connector_id, &endpoint.chat_id)`. For a
    mirror endpoint, `endpoint.thread_id` decides: `Some(thread)` → send to
    `ChatAddress::new(endpoint.chat_id, Some(thread))`; `None` → send to the
    chat with `create_thread: Some(title)`.
  - `route_retitle`: dispatch to mirror endpoints whose `thread_id` is `Some`,
    addressed by `endpoint.address()`; skip the rest.

**Verify**: `cargo build --workspace` → exit 0.

### Step 6: Tests

Update `crates/wuphf-storage/src/tests.rs` and `crates/wuphf-core/src/tests.rs`:

| Old | New |
|---|---|
| `storage.replica_set_for_endpoint(e)` | `storage.conversation_for_endpoint(e).map(|c| c.map(|c| c.id))` |
| `storage.replica_set(id).endpoints` | `storage.conversation(id).unwrap().unwrap().endpoints` |
| `storage.conversation_for_replica_set(id)` | `storage.conversation(id)` |
| `put_replica_set(ReplicaSet { … })` + `put_conversation(…)` | one `put_conversation(Conversation { …, endpoints })` |
| `ReplicaSetId` / `.replica_set_id` | `ConversationId` / `.conversation_id` |

Add new storage tests (run against both backends using the file's existing
generic-function pattern, e.g. the `memory_*` / `sqlite_*` pairs):
- `add_conversation_endpoint` is idempotent and makes
  `conversation_for_endpoint` find the conversation.
- `set_conversation_title` changes the title and leaves `endpoints` intact.

Add a core test: an owner message to a chat outside every rule creates a
conversation with `rule_id == None` containing that one endpoint, and a later
inbound event from that chat is not dropped by `still_covered`.

**Verify**: `cargo test --workspace` → exit 0; total = Step 0 + new tests.

### Step 7: Docs

- `docs/architecture.md` "Core Concepts": delete the "Replica set" bullet;
  change "Conversation — the abstract unit a rule pairs across its two ends
  (one per source chat), backed by one replica set." to "Conversation — the set
  of endpoints that mirror each other, one per source chat a rule matches. A
  message delivered to one endpoint is dispatched to all the others, never back
  to the origin." Replace other "replica set" mentions with "conversation".
- `README.md`: "conversations, replica sets, and mirror threads are minted
  lazily" → "conversations and mirror threads are minted lazily".
- `docs/index.html`: remove the "Replica set" card and fold its sentence into
  the Conversation card; replace other "replica set" mentions.

**Verify**: `grep -rni "replica.set\|ReplicaSet\|ThreadMapping\|thread_mapping" crates docs README.md --include='*.rs' --include='*.md' --include='*.html' | grep -v "alias = \"replica_set_id\"\|legacy"` → no matches.

### Step 8: Pre-deploy check (operator, not CI)

Thread mappings are dropped on the assumption that every mirror endpoint
already carries its thread id. Before deploying, run this against a COPY of
the production database (before migration 0012 runs) and confirm it returns
0 rows:

```sql
SELECT tm.conversation_id, tm.connector_id, tm.thread_id
FROM thread_mappings AS tm
JOIN conversations AS c ON c.id = tm.conversation_id
JOIN replica_sets AS rs ON rs.id = c.replica_set_id
WHERE NOT EXISTS (
    SELECT 1
    FROM json_each(rs.payload, '$.endpoints') AS member
    JOIN endpoints AS e ON e.id = member.value
    WHERE json_extract(e.payload, '$.connector_id') = tm.connector_id
      AND json_extract(e.payload, '$.thread_id') = tm.thread_id
);
```

Write the query and its expected result into the PR description. The
executor cannot run it (no production data); a human must.

### Step 9: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace` → exit 0.

## Test plan

- Migration: `merge_conversations_migration_rewrites_topology` (Step 2) covers
  rule conversations, rule-less owner-message sets, the endpoint index,
  dropped tables, JSON key renames in messages and dispatches, and the stash
  column.
- Storage: new idempotency and title-only-write tests on both backends.
- Core: the existing suite covers lazy minting, mirror thread provisioning and
  reuse, retitles, still-covered checks, and owner sends; plus the new
  rule-less conversation test.
- Model: the plan-027 legacy fixture for `Message` now also proves the
  `replica_set_id` alias.

## Done criteria

- [ ] `cargo test --workspace` exits 0, including the migration test and the new tests
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings
- [ ] `grep -rnw "ReplicaSet\|ReplicaSetId\|ThreadMapping\|replica_set_for_endpoint\|thread_for_conversation\|put_thread_mapping\|conversation_for_thread" crates` → no matches
- [ ] `grep -rn "replica_set_id" crates --include='*.rs'` → matches only inside `alias = "replica_set_id"` attributes, legacy fixtures and the migration test
- [ ] `migrations/0012_merge_conversations.sql` exists and no earlier migration file changed (`git diff --stat -- migrations` lists only the new file)
- [ ] The PR description contains the Step 8 query
- [ ] `plans/README.md` status row updated

## STOP conditions

- The migration test fails on the JSON shape and the fix would change the
  `Conversation` serde shape rather than the SQL.
- `json_extract` / `json_each` are unavailable in the SQLite the tests link
  (error "no such function").
- An existing core test needs a changed assertion (not just renamed fields):
  behavior moved.
- You find an endpoint that belongs to two replica sets in any fixture or
  test (the `conversation_endpoints` primary key assumes one conversation per
  endpoint).
- `persist_provisioned_thread` turns out not to stamp `thread_id` on the
  endpoint in every path that writes a thread mapping.

## Maintenance notes

- Plan 005 (indexed replica-set lookup) is superseded: the
  `conversation_endpoints` table is that index.
- Plan 021 (storage role traits) must be re-planned against the new trait.
- The `#[serde(alias = "replica_set_id")]` attributes can go once no stored
  row uses the old key; after migration 0012, only rows written by an older
  binary after the migration ran could, which should not happen.
- Older plans (003, 007, 017) still say "replica set"; read it as
  "conversation".
- Reviewer focus: the two narrow writers (`add_conversation_endpoint`,
  `set_conversation_title`) are used everywhere a conversation changes after
  creation; `put_conversation` is only used to create one.
