# Dynamic Routing Design

Status: proposed. This document precedes code. It defines a rule-based routing
model that replaces the current static `[[endpoints]]` / `[[groups]]`
configuration, supports threaded fan-out (e.g. every WhatsApp chat mirrored to
its own Telegram forum topic), and tolerates platform identifier migrations
(e.g. WhatsApp phone JIDs to LIDs).

## Goals

- Users declare **mapping rules** instead of hand-listing endpoints and groups.
- Rules express cardinality per side: `single`, `source` (many real chats), or
  `mirror` (one threaded container, one thread per source conversation).
- Multiplicity requires connector thread support; gated by advertised
  capabilities.
- `many` sides filter participating chats by explicit include / exclude ID
  lists.
- Endpoints, conversations, and replication groups are derived and persisted at
  runtime, not configured.
- Platform identifier migrations remap addresses without changing internal
  identity or losing reply/dedup/thread history.

## Non-Goals (deferred)

- Symmetric many-to-many where both sides are real multi-chat with automatic
  pairing. Exactly one `many` side is the identity source; the other is a
  threaded mirror.
- Glob or regex filters. Filters are explicit ID lists only.
- Collapsing many real chats into one chat without threads. Separation always
  uses threads.

## Decisions (locked)

1. Filters are explicit ID lists (`include` / `exclude`), exact match.
2. Separation always uses threads. A `many` side that is not the real-chat
   source must be a threaded container (`mirror`).
3. One `many` side is the identity source (real chats); the paired side is a
   threaded mirror that mints one thread per source conversation. Symmetric
   many-to-many is deferred.
4. Rules replace static `[[endpoints]]` / `[[groups]]`. Both are removed from
   configuration and become runtime-derived persisted state.

## Side Kinds

A rule binds two or more sides. Each side names a connector and a selector.

- `single` — one fixed chat: `chat` plus optional `thread`. Identity is a
  single deterministic endpoint.
- `source` — a set of real chats discovered at runtime, filtered by
  `include` / `exclude`. Each distinct chat is its own conversation. This side
  owns conversation identity.
- `mirror` — one `container` (a forum group / threaded channel). Each source
  conversation is realized as one thread, created on demand. Holds no
  independent conversation identity; it follows the source.

Cardinality emerges from the side kinds:

| Side A | Side B | Result |
|--------|--------|--------|
| single | single | one-to-one (today's group) |
| single | mirror | one chat fanned to threads |
| source | single | each source chat to one shared chat — disallowed (no separation) |
| source | mirror | watgbridge: every source chat to its own thread |

A `source` paired with `single` is rejected at config validation because it
would collapse without separation (decision 2). Separation requires a `mirror`.

## Capability Gate

At registration each connector advertises capabilities:

- `supports_threads`
- `supports_topic_create`

A `mirror` side is legal only if its connector advertises both. WhatsApp
advertises neither, so it may be `single` or `source` but never `mirror`.
Telegram (forum topics) and Discord advertise both. Config load fails if a rule
assigns `mirror` to a connector lacking the capability.

## Configuration Shape

Core configuration keeps top-level transport/media keys and `[[connector]]`
entries (id, platform). Endpoint and group sections are removed. Rules are
added.

```toml
listen = "127.0.0.1:50051"
database_url = "sqlite://wuphf.sqlite"
max_media_bytes = 33554432
orphan_media_retention_seconds = 86400

[[connector]]
id = "whatsapp-main"
platform = "whatsapp"

[[connector]]
id = "telegram-main"
platform = "telegram"

# watgbridge: every allowed WhatsApp chat to its own Telegram forum topic
[[rule]]
id = "wa-mirror"

  [[rule.side]]
  connector = "whatsapp-main"
  kind = "source"
  include = ["111@g.us", "222@g.us"]
  exclude = ["333@g.us"]

  [[rule.side]]
  connector = "telegram-main"
  kind = "mirror"
  container = "-1001234567"

# classic one-to-one
[[rule]]
id = "direct"

  [[rule.side]]
  connector = "whatsapp-main"
  kind = "single"
  chat = "12345@s.whatsapp.net"

  [[rule.side]]
  connector = "telegram-main"
  kind = "single"
  chat = "-100123"
  thread = "12"
```

Per-connector TOML loses its `targets` table and `endpoints` list. A connector
now needs only its core address, connector id, and session/credential state.
Address-to-endpoint resolution moves entirely into core.

## Identity Model Changes

The core already mints internal UUIDs (`EndpointId`, group, message). Platform
addresses become **aliases** pointing to those UUIDs. This is the foundation
for both dynamic provisioning and migration tolerance.

- **Endpoint** = stable internal UUID. A platform address (`chat_id`, optional
  `thread_id`) maps to it through an alias, not by equality.
- **Conversation** = the abstract unit a rule pairs across sides. Backed by one
  derived replication group whose members are the per-side endpoints.
- **Alias** = `(connector_id, platform_address) -> endpoint UUID`, many-to-one.
  Migration adds a new alias for the same UUID. Aliases are never superseded:
  after a migration both the old and new addresses stay live for inbound
  resolution, because WhatsApp may continue sending under either ID.
- **Canonical address** = each endpoint marks one alias as canonical for
  outbound sends. Migration promotes the new address to canonical while leaving
  the old alias resolvable for inbound.

## Connector Protocol Changes

The current model has connectors own a static `TargetRegistry` and pre-resolve
both directions. Under rules this moves into core; connectors become raw pipes
keyed by platform address.

### Inbound

Connector sends raw origin coordinates instead of a pre-resolved endpoint:

```text
InboundMessage {
  connector_id
  chat_id
  thread_id        (optional)
  ... existing normalized payload, attachments, reply info ...
}
```

Core resolves `(connector_id, chat_id, thread_id)` through the alias table to an
endpoint, lazily minting endpoint + conversation + group on first sighting if a
rule matches. Unmatched addresses are dropped.

### Outbound

Core sends a raw target plus optional provisioning instruction:

```text
MessageCreateDispatch {
  target_chat_id
  target_thread_id       (optional, for an existing thread)
  provision_thread {     (optional, when a mirror thread must be created first)
    container
    title
  }
  ... existing payload, reply context, attachments ...
}
```

The connector, if `provision_thread` is set, creates the thread/topic, then
sends, then acknowledges with the native message id **and** the resolved
thread id so core can persist the mapping.

### Capabilities and Migration frames

New connector input frames:

- `Capabilities { supports_threads, supports_topic_create }` at registration.
- `IdentityMigrated { connector_id, old_address, new_address }` when the
  platform signals an ID change (e.g. whatsmeow surfaces a LID mapping). Core
  adds `new_address` as an alias for the same endpoint UUID and keeps the old
  alias resolvable.

### Acknowledgement

Acknowledgement additionally carries the resolved `thread_id` (and, when a
thread was provisioned, its root/topic id) so core records
`conversation -> (container, thread)`.

## Storage Changes

New persisted tables (with a migration):

- `aliases(connector_id, platform_address, endpoint_id, is_canonical)` — unique
  on `(connector_id, platform_address)`, many rows per `endpoint_id`, exactly
  one canonical per endpoint for outbound sends.
- `conversations(id, rule_id, source_address)` — one per source chat (or per
  `single` pair).
- `conversation_endpoints(conversation_id, side_id, endpoint_id)` — derived
  group membership.
- `thread_mappings(conversation_id, connector_id, container, thread_id)` —
  mirror thread per conversation.

The static endpoint/group load path in the CLI is removed. Endpoints and groups
are created lazily by the router/core during resolution and replayed from these
tables on restart. Existing message/copy/dedup/dispatch tables are unchanged;
they already key on internal UUIDs, so migrations of platform addresses do not
touch them.

## Runtime Flow (source-to-mirror)

1. WhatsApp inbound from `111@g.us`. Connector sends raw coordinates.
2. Core matches rule `wa-mirror` source filter (in `include`, not in
   `exclude`).
3. Resolve `(whatsapp-main, 111@g.us)` via aliases. None → mint source endpoint,
   conversation, and group; record the alias.
4. Mirror side is a Telegram container with no thread yet → emit dispatch with
   `provision_thread { container = -1001234567, title = <chat display name> }`.
5. Telegram connector creates the forum topic, sends, acknowledges with native
   message id and topic id. Core persists `thread_mappings` and the mirror
   endpoint + alias.
6. Subsequent messages resolve directly; reverse direction (topic reply to
   WhatsApp) is symmetric through the same conversation group.

## Identifier Migration Flow

1. WhatsApp migrates `12345@s.whatsapp.net` to `67890@lid`.
2. whatsmeow surfaces the mapping; connector sends
   `IdentityMigrated { old = 12345@s.whatsapp.net, new = 67890@lid }`.
3. Core looks up the endpoint UUID for the old alias and inserts a new alias row
   for the new address pointing to the same UUID.
4. Future inbound under either address resolves to the same endpoint,
   preserving conversation, group, thread mapping, reply history, and dedup.
5. Filters: an `include`/`exclude` entry naming the old address still matches,
   because resolution checks all aliases of the resolved endpoint. A rule that
   only ever named the new address matches once the migration alias exists.

Edge case: migration arrives before the endpoint exists (never-seen chat). The
alias is still recorded so a later first sighting resolves correctly; no
conversation is created until a message actually matches a rule.

## Validation Rules (config load)

- Every `rule.side.connector` must reference a declared `[[connector]]`.
- A rule has at least two sides.
- At most one `source` side per rule; `mirror` sides require a paired `source`
  or `single`.
- `mirror` requires the connector's advertised thread capabilities (checked at
  registration; config load checks platform-level capability statically).
- `single.chat` required; `mirror.container` required; `source` may carry
  `include` / `exclude`.
- Reject `source` paired only with `single` (no separation).
- Reject overlapping rules that would map the same address into two
  conversations.

## Blast Radius

- `wuphf-model`: alias, conversation, rule, capability, provision-thread,
  migration types.
- `wuphf-proto`: raw-address inbound, raw-address + provision outbound,
  capabilities frame, migration frame, thread id in acknowledgement,
  `ChatMetadataChanged` inbound, `RetitleThread` outbound.
- `wuphf-storage`: new tables + migration; remove static endpoint/group load
  assumptions; alias resolution and lazy mint helpers.
- `wuphf-router`: rule evaluation, lazy endpoint/conversation/group creation,
  thread provisioning decision.
- `wuphf-core`: identity minting for derived entities, migration handling.
- `wuphf-cli`: new config schema (`[[connector]]`, `[[rule]]`); remove
  `[[endpoints]]`/`[[groups]]` load.
- Telegram connector: delete `TargetRegistry`; raw-pipe inbound/outbound;
  forum topic creation and retitle; advertise capabilities.
- WhatsApp connector: delete `TargetRegistry`; raw-pipe inbound/outbound;
  advertise capabilities; emit `IdentityMigrated` on LID mapping; emit
  `ChatMetadataChanged` on group-subject change.
- Docs: update `architecture.md`, `connector-protocol.md`,
  `implementation-status.md`, connector READMEs, example configs.

## Resolved Behaviour

- **Mirror title tracks source rename.** When a source chat is renamed, the
  thread is renamed to match. The source connector emits a chat-metadata-change
  event (new display name); core issues a `RetitleThread { container, thread,
  title }` dispatch to the mirror connector. Title is also set at creation.
- **Mirror inbound reverse-mapping.** A message typed directly into a mirror
  topic resolves back to its source conversation through `thread_mappings`
  (`(container, thread) -> conversation`). Topics not created by the bridge have
  no mapping and are ignored.
- **Migration is old→new only.** If a platform ever reverses a migration, the
  same `IdentityMigrated` frame with swapped arguments handles it; no extra
  mechanism needed.

## Protocol Additions From Resolved Behaviour

- Inbound `ChatMetadataChanged { connector_id, chat_id, title }` so a source
  rename can propagate.
- Outbound `RetitleThread { container, thread, title }` dispatch for the mirror
  connector. Requires `supports_topic_create` (same capability that allows topic
  management).
