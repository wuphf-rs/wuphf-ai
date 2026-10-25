# Architecture

wuphf replicates messages between chat platforms by modelling **one logical
message and its copies**, rather than blindly forwarding platform messages. All
identity, routing, and loop-prevention logic lives in the Rust core; connectors
are thin pipes.

## Core Concepts

- **Logical message** — every message has a core-owned `MessageId`. Each
  delivered or received platform representation is a `MessageCopy` keyed by
  `(MessageId, EndpointId, native_id)`.
- **Endpoint** — an internal handle for one chat (optionally one thread) on one
  connector. Connectors never see endpoint ids; they report raw platform
  addresses, and the core resolves an address to an endpoint through an
  **`Alias`**. Many aliases can point at one endpoint (e.g. a WhatsApp
  phone-to-LID migration), with exactly one canonical for outbound sends.
- **Conversation** — the set of endpoints that mirror each other, one per
  source chat a rule matches. A message delivered to one endpoint is
  dispatched to all the others, never back to the origin.


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

Canonical forms are for identity only. On the way out, the WhatsApp connector
puts every address back into the form the chat is addressed in, because WhatsApp
keys a chat's messages by that form: a quote naming its author in the other form
arrives unlinked from its parent. A group's form is the addressing mode its
messages carry (or, before any arrives, its metadata); a direct chat's is the
form its messages arrive on, except that an account WhatsApp has migrated to LIDs
sends every direct message to the LID. The dispatch target, the quoted author
(including the paired account itself, which a LID chat knows by its LID) and the
chat in edit and delete keys all follow it.
## Routing Rules

Rules are the only routing input a user writes; conversations and endpoints
are derived lazily at runtime. A `Rule` declares a flow
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

### Runtime settings

`rules` and `notify` can change while the core runs. `wuphf-settings` keeps a
botconf settings store over the file, the `WUPHF_CORE_*` environment and the
overrides stored in SQLite; a change is validated like the file (the whole
configuration is rebuilt and checked, including `RoutingConfig::validate`) and
an invalid one is rejected and never stored. Every other key — the connectors,
`listen`, `database_url`, the tokens and `functions` — is read once at startup.

The core reads its routing config through a `RoutingSource` once at the start
of each operation, so one event routes against one consistent config and the
next event sees a change. Conversations remember their rule by id, so removing
a rule stops its conversations (re-adding the same id resumes them), and
renaming a rule starts new conversations with new mirror threads. Editing a
rule in place does not move existing conversations: their endpoints are
already stored and are reused.

## Data Flow

```text
Connector --Event (raw address)--> event bus --> Core --> Router
         --DispatchCommand (resolved target)--> event bus --> Connector
```

Connectors normalize platform input tagged with its native chat/thread address
and execute platform API actions. They never decide membership, fan-out,
dedup, reply mapping, loop prevention, or address mapping.

## Components

| Crate | Responsibility |
|-------|----------------|
| `wuphf-model` | Shared domain types, grouped into `ids` / `routing` / `connector` / `message` / `dispatch`. |
| `wuphf-storage` | The `Storage` trait — the sum of six role traits (topology, messages, media, dispatch queue, provisioning stash, observation log) — plus two backends: `memory` (tests) and `sqlite` (durable). A consumer that needs only part of storage bounds on its roles. The same database holds runtime settings overrides (`settings` table, the `SettingsOverrides` role, outside `Storage`), read through `wuphf-settings`. |
| `wuphf-core` | Owns logical identity, dedup, echo suppression, reply resolution and identity migration; evaluates rules and computes dispatch targets (`config.rs`, `router.rs`). |
| `wuphf-runtime` | Tokio event bus + dispatch pipeline; connector session hub; bounded media broker; gRPC transport adapter. |
| `wuphf-functions` | Brokers remote function calls between callers and provider processes; independent of the core and storage. |
| `wuphf-settings` | Loads the core configuration (TOML, then `WUPHF_CORE_*` environment) and describes it to botconf; builds the routing config, connector list, resolved functions and connector tokens from it. |
| `wuphf-proto` | Canonical protobuf contract and Tonic bindings. |
| `wuphf-cli` | Loads the configuration through `wuphf-settings`, wires storage/core/runtime, serves gRPC. |
| `connectors/` | Go connectors: Telegram (`gotd/td`, forum topics), WhatsApp (`whatsmeow`, QR pairing), and Instagram (`mautrix/meta`, cookie auth). |

## Replicated Operations

- **Create** — fanned out to every other endpoint in the conversation. The
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


## Owner Notices

A connector that loses its session stops bridging without failing: whatsmeow
keeps the process alive after a WhatsApp logout and simply delivers nothing,
which from the outside is indistinguishable from nobody writing to you. Rather
than have every connector decide how to raise the alarm, a connector reports
*what happened* — an `OwnerNotice` carrying a `NoticeKind` (logged out, auth
required, degraded, recovered) plus platform detail — and the core decides the
rest.

The core composes the wording, names the connector in it (the notice arrives
somewhere unrelated to the connector in trouble), and delivers it to the chats
configured under `[[notify]]` through the ordinary raw-send path, so a notice is
a durable dispatch that survives a restart and is redelivered if its own target
connector is offline.

Two rules make it useful rather than noisy:

- **Never through the connector that raised it.** A logged-out WhatsApp cannot
  deliver the news that it is logged out, so targets on the reporting connector
  are skipped. Configuring targets on more than one platform is what keeps the
  owner reachable when one is down.
- **The same state is reported once.** A connector in a reconnect loop repeats
  itself; the owner is told on a change and then not again until the state
  changes. Suppression is by state rather than elapsed time, which keeps the core
  clock-free. A recovery is only news after trouble, so a healthy start-up says
  nothing. The state is in memory, so a restart re-notifies about a connector
  that is still down — the moment the owner most wants to know.
## Media

The runtime exposes a bounded media broker. A source connector uploads bytes
once; normalized attachments and dispatches refer only to a core-owned
`MediaId`. The destination connector downloads by id while executing its
dispatch, so platform-private file handles never become cross-platform message
identity. Uploads not linked into a logical message are pruned after
`orphan_media_retention_seconds`; linked media is retained.

## Remote Functions

Some work, such as sticker conversion, needs heavy native tooling that should
not be compiled into every connector or rewritten in each connector language.
Provider processes, written in any language, serve it as named functions
instead. The core is only a broker: a provider dials in and registers the
functions it is configured to serve, and every call travels from the caller to
the core, on to the provider, and back the same way. Providers never listen.

A call carries a small JSON body plus a list of media ids; bulky data moves
through the media broker, never through the call itself. When no provider for a
function is connected, a call waits for one until its deadline, and a name that
no configured provider may serve fails at once. The deadline is the caller's
own, capped by the core (or the core's default when the caller sets none), and
time spent waiting counts against it. A caller that gives up cancels the call at
the provider. A provider lost mid-call has its calls re-queued when the function
is declared idempotent and failed otherwise, and a provider reconnecting under
its id takes over its old session.

The first function is `media.convert@1`, served by `providers/mediaconv`. It
converts animated stickers between the formats platforms use (WhatsApp's
animated WebP, Telegram's WebM and Lottie stickers), so the Telegram and
WhatsApp connectors carry no conversion tools; without a converter they fall
back to a static sticker or a file.

Only processes outside the core call functions: routing, dispatch and
acknowledgement never wait on a provider, and the broker crate
(`wuphf-functions`) depends on neither the core nor storage.

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
- A connector session is trusted only when it presents that connector's token,
  and a connector has at most one live session: a second one is refused rather
  than taking over the first.

## Planned

- Symmetric N-to-N rules (every side both source and mirror).
- Per-side replication direction (one-way mirrors).
- Instagram outbound attachments (the connector sends text only).
- Broaden media kinds and define a media-deletion policy.
