# Architecture

wuphf replicates messages between chat platforms by modelling **one logical
message and its copies**, rather than blindly forwarding platform messages. All
identity, routing, and loop-prevention logic lives in the Rust core; connectors
are thin pipes.

## Core Concepts

- **Logical message** — every message has a core-owned `MessageId`. Each
  delivered or received platform representation is a `MessageCopy` keyed by
  `(MessageId, EndpointId, platform_message_id)`.
- **Endpoint** — an internal handle for one chat (optionally one thread) on one
  connector. Connectors never see endpoint ids; they report raw platform
  addresses, and the core resolves an address to an endpoint through an
  **`Alias`**. Many aliases can point at one endpoint (e.g. a WhatsApp
  phone-to-LID migration), with exactly one canonical for outbound sends.
- **Replica set** — the set of endpoints that mirror each other. A message
  delivered to one member is dispatched to all the others, never back to the
  origin.
- **Conversation** — the abstract unit a rule pairs across sides (one per source
  chat), backed by one replica set.

## Routing Rules

Rules are the only routing input a user writes; conversations, endpoints, and
replica sets are derived lazily at runtime. A `Rule` pairs two or more
`RuleSide`s, each naming a connector and a `SideKind`:

- `single` — one fixed chat, optionally one thread.
- `source` — every real chat on the connector, filtered by explicit
  include/exclude id lists. Owns conversation identity.
- `mirror` — one threaded container; the core provisions one thread per source
  conversation on demand. Legal only on a connector that advertises both
  `supports_threads` and `supports_topic_create`.

The canonical topology is **source → mirror** (e.g. every WhatsApp chat mirrored
into its own Telegram forum topic). A `source` paired only with `single` is
rejected because it would collapse chats without separation.

## Data Flow

```text
Connector --NormalizedEvent (raw address)--> event bus --> Core --> Router
         --DispatchCommand (resolved target)--> event bus --> Connector
```

Connectors normalize platform input tagged with its native chat/thread address
and execute platform API actions. They never decide membership, fan-out,
dedup, reply mapping, loop prevention, or address mapping.

## Components

| Crate | Responsibility |
|-------|----------------|
| `wuphf-model` | Shared domain types, grouped into `ids` / `routing` / `connector` / `message` / `dispatch`. |
| `wuphf-storage` | The `Storage` trait plus two backends: `memory` (tests) and `sqlite` (durable). |
| `wuphf-router` | Evaluates rules; resolves or lazily provisions endpoints/conversations/threads; computes dispatch targets; resolves per-target delivery policy. |
| `wuphf-core` | Owns logical identity, dedup, echo suppression, reply resolution, identity migration; queues routing work. |
| `wuphf-runtime` | Tokio event bus + dispatch pipeline; connector session hub; bounded media broker; gRPC transport adapter. |
| `wuphf-proto` | Canonical protobuf contract and Tonic bindings. |
| `wuphf-cli` | Loads TOML config, wires storage/core/runtime, serves gRPC. |
| `connectors/` | Go connectors: Telegram (`gotd/td`, forum topics), WhatsApp (`whatsmeow`, QR pairing), and Instagram (`mautrix/meta`, cookie auth). |

## Replicated Operations

- **Create** — fanned out to every other endpoint in the replica set, carrying
  the target's resolved `DeliveryOptions` so the connector renders attachments
  and over-length text per policy.
- **Edit / Delete** — resolved to the original copies and fanned out. How each
  target renders them is governed by per-connector **`DeliveryOptions`**:
  `edit_mode` (`in_place` | `repost`), `delete_mode` (`revoke` | `tombstone`),
  `attachments_as_files` (send every attachment as a generic file), and
  `max_message_length` (spill over-length text into a plain-text file). Options
  are configured on each connector and resolved against the *target* connector at
  routing time, so policy is connector-specific and directional — never a
  symmetric property of a replica set.
- **Reactions** are modelled but not yet executed.

## Media

The runtime exposes a bounded media broker. A source connector uploads bytes
once; normalized attachments and dispatches refer only to a core-owned
`MediaId`. The destination connector downloads by id while executing its
dispatch, so platform-private file handles never become cross-platform message
identity. Uploads not linked into a logical message are pruned after
`orphan_media_retention_seconds`; linked media is retained.

## Key Invariants

- An origin endpoint is never a dispatch target for its own event.
- Duplicate inbound event keys are ignored; copies recorded as bridge-sent are
  ignored if echoed back.
- Platform message ids live only at copy boundaries, never as logical identity.
- A reply dispatch is blocked until the parent's copy exists at the target
  endpoint, then released with the native parent id.
- Pending dispatches survive restart; unacknowledged work is redelivered when a
  connector reconnects.
- A dispatch the connector reports as permanently unsendable is dead-lettered
  (`Failed`) instead of being redelivered forever.
- A mirror conversation's thread is provisioned on first dispatch and reused.
- A mirror thread's title tracks its source conversation: the core records the
  source title and fans a retitle dispatch to provisioned threads on a change.
- Inbound attachments are accepted only after their media and metadata match a
  broker-issued record.

## Planned

- Symmetric N-to-N rules (every side both source and mirror).
- Execute reactions; broaden media kinds and define a media-deletion policy.
