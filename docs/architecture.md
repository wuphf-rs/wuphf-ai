# System Architecture

## Scope

`wuphf` is a durable messaging replication engine. The first MVP is a runnable
mock-backed system: a Rust core service, its language-neutral gRPC contract,
SQLite-backed state, and a Rust mock connector. It does not implement
production WhatsApp, Telegram, Discord, or other platform-specific
integrations.

## Components

### Connectors

A connector instance owns communication with one platform account or the mock
environment. It may be written in any language capable of using the gRPC
contract. It:

- Converts native incoming events into normalized protocol events.
- Authenticates and maintains one bidirectional gRPC stream to the core.
- Executes a dispatch command received from the core.
- Reports dispatch success with the resulting platform message ID, or reports
  a dispatch failure.

A connector does not make routing decisions, persist cross-endpoint identity
mappings, or apply retry and replication policy.

### Rust Core

The core is the bridge authority. It:

- Loads configured connectors, endpoints, and replication groups.
- Authenticates connector instances.
- Resolves normalized events into logical messages.
- Determines target endpoints and creates durable dispatch commands.
- Records platform message copies after acknowledgement.
- Implements deduplication, loop prevention, ordered delivery, retries, and
  reply dependencies.
- Tracks dispatch attempts and applies configured bounded exponential backoff
  for transient failed delivery outcomes.
- Provides a core-owned operator command path for inspecting durable
  dispatches and reopening terminal failures.

### Storage

SQLite is required in the first MVP, rather than an in-memory substitute,
because message-copy identity mappings and pending dispatch commands must
survive a process restart. The persistence layer records enough state to
distinguish already-ingested events, retry outstanding work, and accept
idempotent acknowledgement processing.

Persistence changes must be introduced through migrations.

The core enables SQLite foreign-key enforcement, WAL journaling, full
synchronous durability, and a bounded busy timeout when opening durable state.
It currently uses one pooled connection because dispatch sequence allocation
and claims are serialized by design.

On process shutdown signals, the gRPC server performs graceful shutdown rather
than immediately terminating active connector streams.

### Operations And Future API

The MVP includes narrow local operator commands to list durable dispatches and
reopen a terminal failure through core-owned repository operations. A later
authenticated live administration API or TUI may show endpoints, replication
groups, connector health, and backlog metrics. It must interact with
core-owned state and policy rather than transferring bridge decisions into a
UI or connector process.

## Domain Model

- **Endpoint**: a configured addressable destination or origin within a
  connector integration, such as a chat identifier interpreted by that
  connector.
- **ReplicationGroup**: a configured set of endpoints among which eligible
  messages are replicated.
- **Message**: the logical message created from a normalized origin event,
  independent of any one platform's identifier.
- **MessageCopy**: the association between a logical message, an endpoint, and
  the platform message ID created or observed at that endpoint.
- **Pending dispatch**: durable state representing a dispatch command that
  should be sent, retried, acknowledged, failed, or blocked by a dependency.
- **Connector instance**: the configured authenticated participant responsible
  for one connector integration and its endpoints.
- **Deduplication record**: durable evidence that a submitted connector event,
  or an acknowledged result, has already been processed.

Protocol-facing text uses **dispatch command** for work delivered to a
connector; storage may represent that work as a pending dispatch.

## Routing Pipeline

1. A connector instance registers and authenticates over its long-lived gRPC
   stream.
2. The connector submits a normalized `CreateMessage` for an origin endpoint,
   including its connector event ID and platform message ID.
3. The core authenticates the origin, applies deduplication, and records the
   origin message copy for a newly resolved logical message.
4. The core resolves replication group membership and creates durable dispatch
   commands for eligible target endpoints.
5. A dispatch command whose dependencies are satisfied is delivered to the
   target connector instance in per-endpoint order.
6. The connector creates the platform-side message and acknowledges the
   dispatch with the resulting platform message ID, or reports failure.
7. The core durably records a successful target message copy and completes the
   pending dispatch. Failure or disconnect leaves durable state available for
   retry according to core policy.

## Loop Prevention And Idempotency

Replicated messages must not circulate indefinitely. The core owns the stable
logical message to message copy mapping, and a connector-originated event that
corresponds to an already recorded dispatched copy must not produce a new
logical message and new fan-out. Connector event IDs also allow repeated
submission of the same normalized platform event to be handled once.

Dispatch delivery is retryable. An acknowledgement received again after a
reconnect or ambiguous network outcome must converge on the same message copy
and completed dispatch state rather than creating another delivery. These
invariants belong in persistence and tests, not only in connection-local
memory.

A delivered but unacknowledged command is leased until its configured
acknowledgement timeout before it can be emitted again during an active
session. A reconnect may immediately reclaim unfinished work. A connector may
report a permanent failure; that dispatch is durably terminal and continues
to block later commands for the same endpoint until an operator explicitly
reopens it through the core command path.

## Reply Dependency

A reply is related to a parent logical message, but each platform needs its
local parent platform message ID to create the corresponding reply. Therefore,
a reply dispatch command for a target endpoint must remain blocked until the
core has recorded the parent message's acknowledged message copy at that same
endpoint. Once the parent copy is available, the core may provide its platform
message ID in the dependent dispatch.

## MVP Boundary

The first MVP includes normalized create-message events, durable
create-message dispatch, configured authentication, attachment metadata, reply
dependencies, and mock-client testing. Attachment descriptors identify
image, video, audio, file, or sticker content; no media bytes cross the
protocol in v1.

Real platform connectors, including WhatsApp to Telegram replication and
Telegram topic mapping, remain the next platform milestone. Edits, reactions,
deletes or revokes, media transfer, live management interfaces, dynamic group
administration, mTLS, and multi-session management remain later work.
