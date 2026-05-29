# Architecture

## Identity Model

The core owns a `MessageId` UUID for every logical message. Each delivered or
received platform representation is a `MessageCopy` identified by:

```text
(logical MessageId, EndpointId, platform_message_id)
```

Replies use internal `MessageId` references. Before dispatching a reply to an
endpoint, the router resolves the parent's copy at that endpoint and supplies
the target platform's native reply identifier.

## Data Flow

```text
Connector -> NormalizedEvent -> Tokio event bus -> Core -> Router
          -> DispatchCommand -> Connector
```

Connectors normalize platform input and perform platform API actions. They do
not decide group membership, fan-out, deduplication, reply mapping, or loop
prevention.

## Implemented Boundary

`wuphf-model` owns cross-layer types. `wuphf-storage` abstracts state and
ships in-memory and SQLite implementations. SQLite migrations preserve
entities, copy mappings, deduplication claims, and durable dispatch state.
`wuphf-router` calculates dispatch targets and blocks replies whose parent
lacks a target-side copy. `wuphf-core` assigns identity and queues routing
work. `wuphf-runtime` moves events, acknowledgements, and dispatches
asynchronously through Tokio channels. Its connector hub binds registered
endpoint ownership to configured connector sessions without adding platform
logic. `wuphf-proto` and Tonic adapter expose session boundary over
bidirectional gRPC. The runtime also exposes a bounded media broker:
connectors upload bytes once, while normalized attachments and dispatches
refer to a core-owned `MediaId`. Storage records which media IDs are attached
to logical messages, allowing expired abandoned uploads to be removed without
breaking replayable dispatches.

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
- A connector acknowledgement stores sent copy identity and releases blocked
  replies for that endpoint.
- A connector session can submit events only for owned endpoints and
  acknowledge only assigned dispatches.
- Connector registration must match configured connector/platform/endpoint
  assignment before work can be received.
- Attachment data is addressed by core-owned media identity; connector-private
  platform file handles do not escape adapter boundaries.
- The core accepts attachment events only after their media and metadata match
  a broker-issued media record.

## Planned Extension Points

- Implement the remaining normalized payload operations for edits, deletes,
  and reactions.
- Extend connector media kinds and define deletion policy for media attached
  to retained logical messages.
- Carry enough target-copy quote metadata for correct outbound WhatsApp
  replies.
- Harden live connector retry/reconnect behavior and integration tests.
