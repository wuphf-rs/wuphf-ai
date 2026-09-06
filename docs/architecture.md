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
- **Conversation** — the abstract unit a rule pairs across its two ends (one
  per source chat), backed by one replica set.


## Address Identity

Platforms hand out several interchangeable addresses for one chat. WhatsApp
reaches a contact as a phone JID (`<number>@s.whatsapp.net`), a hidden LID
(`<number>@lid`), and per-device AD-JIDs, and switches between them from one
message to the next. Left alone, each form mints its own endpoint and one chat
splits across several mirror threads.

Two mechanisms keep that variance out of the rest of the core:

- **Canonicalization at the connector.** The source connector resolves every
  address it reports to one stable form (device stripped, phone promoted to LID),
  so the core sees one address per chat within a session.
- **Address links.** When a platform reveals that two addresses name the same
  chat, the connector reports an identity migration and the core records the
  pair in `address_links` — symmetric, and independent of endpoints, so a link
  is kept even for a chat that has never been routed.

Everything that keys on an address goes through one of them. A rule's chat list
is matched against an `AddressSet` (the address an event arrived on plus the
transitive closure of its links), so a chat written by phone number is selected —
or, in a `not` list, excluded — however the platform addressed it. Event dedup
and the retitle check key on the *endpoint's* address rather than the raw one,
which makes them address-agnostic for free. The observation log is the one layer
written before resolution (so it can capture chats no rule matches), so a
migration re-keys its rows onto the new address to heal a chat already split
across both forms.

A rule can only recognize a chat by a form the platform has actually reported.
Where no counterpart is known — the LID store has no mapping and WhatsApp sends
no alternate address — a chat listed under the other form stays unmatched.
## Routing Rules

Rules are the only routing input a user writes; conversations, endpoints, and
replica sets are derived lazily at runtime. A `RoutingRule` declares a flow
between a source end and a destination end. The source end owns conversation
identity; replication itself is two-way.

- `one-to-one` — one fixed source chat mirrored into one fixed destination
  chat (optionally a thread within it).
- `many-to-one` — every chat on the source connector that passes the
  `source_chats_filter` (`all` by default, or an `only` allowlist / `not`
  blocklist of chats), each mirrored into its own thread provisioned on demand
  inside the destination container chat. Legal only when the destination
  connector advertises both `supports_threads` and `supports_topic_create`.

The canonical topology is **many-to-one** (e.g. every WhatsApp chat mirrored
into its own Telegram forum topic).

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
| `wuphf-router` | Evaluates rules; resolves or lazily provisions endpoints/conversations/threads; computes dispatch targets. |
| `wuphf-core` | Owns logical identity, dedup, echo suppression, reply resolution, identity migration; queues routing work. |
| `wuphf-runtime` | Tokio event bus + dispatch pipeline; connector session hub; bounded media broker; gRPC transport adapter. |
| `wuphf-proto` | Canonical protobuf contract and Tonic bindings. |
| `wuphf-cli` | Loads TOML config, wires storage/core/runtime, serves gRPC. |
| `connectors/` | Go connectors: Telegram (`gotd/td`, forum topics), WhatsApp (`whatsmeow`, QR pairing), and Instagram (`mautrix/meta`, cookie auth). |

## Replicated Operations

- **Create** — fanned out to every other endpoint in the replica set. The
  dispatch carries content only; how a copy is rendered (attachments-as-files,
  over-length text spillover) is each connector's own `[rendering]`
  configuration.
- **Edit / Delete** — resolved to the original copies and fanned out. How each
  target renders them (`edit_mode`: `in_place` | `repost`; `delete_mode`:
  `revoke` | `tombstone`) is likewise the target connector's own `[rendering]`
  configuration, so policy stays connector-specific and directional without the
  core shipping it.
- **Reactions** — fanned out like edits, carrying the reactor and emoji
  (empty = cleared); the target connector decides how to display them.
- **Delivery / read receipts** — emitted as their own signals rather than as
  reactions, so a connector can render them separately (Telegram: 👍 delivered,
  👀 read in DMs). A connector that renders neither acknowledges them unchanged.
- **Attachment groups** — a message carrying several attachments is delivered as
  one album where the platform has one (Telegram, capped at its limit of 10),
  and otherwise as consecutive messages with the caption on the first, whose id
  becomes the recorded copy.

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
- Per-side replication direction (one-way mirrors).
- Instagram outbound attachments (the connector sends text only).
- Broaden media kinds and define a media-deletion policy.
