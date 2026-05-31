# Architecture

## Identity Model

The core owns a `MessageId` UUID for every logical message. Each delivered or
received platform representation is a `MessageCopy` identified by:

```text
(logical MessageId, EndpointId, platform_message_id)
```

An `EndpointId` is an internal, lazily-minted handle for one concrete chat (or
thread) on one connector. Connectors never see endpoint IDs: they report raw
platform addresses, and the core resolves an address to an endpoint through an
`Alias`. Many aliases may point at one endpoint — for example after a WhatsApp
phone-to-LID migration both addresses resolve to the same endpoint — and
exactly one alias is canonical for outbound sends.

Replies use internal `MessageId` references. Before dispatching a reply to an
endpoint, the router resolves the parent's copy at that endpoint and supplies
the target platform's native reply identifier.

## Routing Rules

Routing is configured as a set of `Rule`s, each pairing two or more `RuleSide`s.
A side names a connector and a `SideKind`:

- `Single { chat_id, thread_id? }` — one fixed chat, optionally one thread
  within it.
- `Source { include, exclude }` — every real chat on the connector, filtered by
  explicit ID lists. A source owns conversation identity.
- `Mirror { container }` — one threaded container in which the core provisions
  one thread per source conversation. Only legal on a connector whose declared
  capabilities include both `supports_threads` and `supports_topic_create`.

A `Conversation` is the abstract unit a rule pairs across sides, backed by one
`ReplicationGroup` and anchored to its source connector/address. When a source
event first matches a rule, the core lazily mints the endpoint, conversation,
and group. For a mirror side it provisions a thread on demand and records the
result as a `ThreadMapping (conversation, connector, container, thread_id)`, so
later messages in that conversation reuse the same thread.

## Data Flow

```text
Connector -> NormalizedEvent (raw SourceAddress) -> Tokio event bus -> Core
          -> Router -> DispatchCommand (resolved TargetAddress) -> Connector
```

Connectors are raw pipes: they normalize platform input tagged with its native
chat/thread address and perform platform API actions. They do not decide group
membership, fan-out, deduplication, reply mapping, loop prevention, or any
address mapping.

## Implemented Boundary

`wuphf-model` owns cross-layer types. `wuphf-storage` abstracts state and
ships in-memory and SQLite implementations. SQLite migrations preserve
entities, aliases, conversations, thread mappings, copy mappings, deduplication
claims, and durable dispatch state. `wuphf-router` evaluates rules, resolves or
lazily provisions endpoints/conversations/threads, calculates dispatch targets,
and blocks replies whose parent lacks a target-side copy. `wuphf-core` assigns
identity, applies identity migrations, and queues routing work. `wuphf-runtime`
moves events, acknowledgements, and dispatches asynchronously through Tokio
channels. Its connector hub binds registered connector sessions to their
declared capabilities without adding platform logic. `wuphf-proto` and the
Tonic adapter expose the session boundary over bidirectional gRPC. The runtime
also exposes a bounded media broker: connectors upload bytes once, while
normalized attachments and dispatches refer to a core-owned `MediaId`. Storage
records which media IDs are attached to logical messages, allowing expired
abandoned uploads to be removed without breaking replayable dispatches.

## Runtime Invariants

- An origin endpoint is never a dispatch target for its own event.
- Duplicate inbound platform event keys are ignored.
- Platform copies recorded as bridge-sent are ignored if echoed back.
- Platform message identifiers are used only at copy boundaries, not as
  logical identities.
- A reply dispatch requires the parent message copy at the target endpoint.
- Pending dispatches survive restart and are replayed when runtime starts.
- Unacknowledged assigned dispatches are redelivered when a connector session
  reconnects.
- A connector acknowledgement stores sent copy identity, records any
  provisioned thread mapping, and releases blocked replies for that endpoint.
- An identity migration links a new platform address to the existing endpoint
  and makes it the canonical alias, keeping both old and new addresses
  resolvable.
- Connector registration must match the configured connector ID and platform;
  declared capabilities gate whether the connector may host a mirror side.
- A mirror conversation's thread is provisioned on first dispatch and reused
  for all later messages in that conversation.
- Attachment data is addressed by core-owned media identity; connector-private
  platform file handles do not escape adapter boundaries.
- The core accepts attachment events only after their media and metadata match
  a broker-issued media record.

## Planned Extension Points

- Retitle a provisioned mirror thread when its source conversation is renamed
  (`RetitleThread` dispatch + chat-metadata-changed event). See
  `dynamic-routing-design.md`.
- Symmetric N-to-N rules (every side both a source and a mirror); current rules
  pair one identity-source side with threaded-mirror sides.
- Implement the remaining normalized payload operations for edits, deletes,
  and reactions.
- Extend connector media kinds and define deletion policy for media attached
  to retained logical messages.
- Harden live connector retry/reconnect behavior and integration tests.
