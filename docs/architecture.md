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
- `many-to-one` — every chat on the source connector (or only those in
  `only_chats`, or all but those in `except_chats`), each mirrored into its
  own thread provisioned on demand inside the destination container chat. Legal
  only when the destination connector advertises both `supports_threads` and
  `supports_topic_create`.

The canonical topology is **many-to-one** (e.g. every WhatsApp chat mirrored
into its own Telegram forum topic).

Rules are keyed by id (`[rules.<id>]`). When several rules match the same
chat, the one with the lowest `priority` (default 0) wins, then the lowest id.
Every chat the config names is one string, `connector:chat` or
`connector:chat#thread`: the connector is everything before the first `:`
(connector ids may not contain `:`, `#`, `%` or whitespace; chat ids often
contain `:`), and inside the chat and the thread `#` is written `%23` and `%`
is written `%25`. Only the config uses this form; stored JSON keeps
`ChatRef`'s fields.

### Runtime settings

`rules` and `notify` can change while the core runs. Each rule is its own
entry, `rules.<id>`, set or removed on its own. `wuphf-settings` keeps a
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

### Settings service

The settings store lives in the core, and the core serves it as botconf's
generic `botconf.v1.Settings` gRPC service (`botconf-grpc`), next to its other
services: reading and changing settings, and a settings panel
(`botconf-panel`) that platforms render as buttons. The TUI, a CLI and the
connectors' panels are its clients; none of them decides who may change what.

- A connector authenticates with its own connector token and says who acted
  (the request's actor). The core allows the request only when that platform
  user id is one of **that** connector's `owner_ids`, a file-only list per
  `[[connectors]]` entry. A connector cannot act for another connector's
  owner, and a request with no actor is refused.
- The observer token, which the TUI already holds to send messages as the
  owner, acts as the owner and needs no actor. With no observer token set,
  nothing is accepted as the observer.
- Changes are recorded in the settings table as `<connector>:<actor>`, or
  `observer`.

Chats, connectors and rules in the settings are references (`wuphf.chat`,
`wuphf.connector`, `wuphf.rule`), so the panel shows names. A chat's name is
the title the observation log last saw for it (under its address or a linked
form, such as a phone JID for a LID), else the title of the conversation it is
the source of; a mirror thread is named after its conversation. A chat no one
has written in since the observation log started shows its raw id. Adding a
chat offers the chats the core has seen, most recent first.

Panel buttons carry their action when it fits the platform's button data, and
otherwise a short handle stored in the `panel_tokens` table for 30 days
(renewed whenever a screen draws it again), so they keep working after the
core restarts.

`wuphf settings` (`wuphf-cli`) is the terminal client. `list`, `get`, `set`,
`unset`, `add` and `remove` open the same `WuphfSchema` store as `serve`, over
the core's database, without starting anything else: they validate exactly as
the running core does and work while it is down. A running core keeps its own
copy of the settings, so `wuphf settings reload` asks it to re-read them
through the service's `Reload`, as the observer (`WUPHF_OBSERVER_TOKEN`).
A reload re-reads the config file, the environment and the stored overrides;
an edit that makes the configuration invalid is refused with the reason, and
the running configuration stays. `listen`, `database_url`, the connector list,
the connector tokens and `[functions]` are read only when the core starts, so
changing them still needs a restart.

Connectors declare their own live-safe settings (rendering, sticker metadata,
log level, the sticker conversion timeout) when they register: a botconf
descriptor, a JSON Schema and their file values. The core registers each as a
botconf section at `connector_settings.<id>` (`connector_settings` in
`wuphf-runtime`), validates changes with the schema (the `jsonschema` crate),
remembers the declaration in `connector_settings_declarations` so the section
exists before the connector reconnects, and pushes changes back through
`Watch`, which a connector may follow for its own section without an actor.
The Go connectors do this through `connectors/shared/settings`, the Rust ones
through the connector SDK's `settings` module (`crates/wuphf-connector`),
which talks to the service through `botconf-proto` only.
See docs/connector-protocol.md, "A connector's own settings".

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
| `wuphf-model` | Shared domain types, grouped into `ids` / `routing` / `connector` / `message` / `dispatch` / `chat`. |
| `wuphf-storage` | The `Storage` trait — the sum of seven role traits (topology, messages, media, dispatch queue, provisioning stash, observation log, chat profiles) — plus two backends: `memory` (tests) and `sqlite` (durable). A consumer that needs only part of storage bounds on its roles. The same database holds runtime settings overrides (`settings` table, the `SettingsOverrides` role, outside `Storage`), read through `wuphf-settings`, and the settings panel's button handles (`panel_tokens`, the `PanelTokens` role, also outside `Storage`). |
| `wuphf-core` | Owns logical identity, dedup, echo suppression, reply resolution and identity migration; evaluates rules and computes dispatch targets (`config.rs`, `router.rs`). |
| `wuphf-runtime` | Tokio event bus + dispatch pipeline; connector session hub; bounded media broker; gRPC transport adapter; the settings service's authorizer and reference names (`settings_auth`). |
| `wuphf-functions` | Brokers remote function calls between callers and provider processes; independent of the core and storage. |
| `wuphf-settings` | Loads the core configuration (TOML, then `WUPHF_CORE_*` environment) and describes it to botconf; builds the routing config, connector list, resolved functions and connector tokens from it. |
| `wuphf-proto` | Canonical protobuf contract and Tonic bindings. |
| `wuphf-cli` | Loads the configuration through `wuphf-settings`, wires storage/core/runtime, serves gRPC; `wuphf settings` views and changes the runtime settings from a terminal. |
| `connectors/` | Go connectors: Telegram (`gotd/td`, forum topics), WhatsApp (`whatsmeow`, QR pairing), and Instagram (`mautrix/meta`, cookie auth). |
| `misc/` | `wuphf-misc`: non-bridging features (the Instagram downloader), a client of the observer and function services. |

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

## Chat Profiles

The core keeps what it knows about each platform chat apart from routing in a
chat profile (`chat_profiles` table, the `ChatProfileStore` role): a JSON
payload keyed by the connector and the chat's native id, never by endpoint,
so a later field needs no schema change. A profile holds the chat's display
title, description, avatar (the platform's picture id, the uploaded media and
the core's SHA-256 of its bytes), member count and disappearing-messages
timer. It never holds the member list.

- **Reported by connectors.** A `MessageCreateEvent` carries the timer the
  message had (only when greater than 0); a `ChatUpdateEvent` reports chats in
  a batch (a timer setting, a group notification, a history sync). Reports are
  merged by setting timestamp: with one on both sides the newer wins, otherwise
  the later report wins. A message without a timer, from a connector that
  declares `supports_disappearing_timers`, settles a chat whose timer is still
  unknown as off; it never overrides a known timer.
- **Recorded before routing.** The ingest path records the timer before it
  resolves the origin, so a chat no rule matches still learns it, and a later
  owner reply into it carries it.
- **Read when a send is routed.** Every routed send and every owner send to a
  connector that declares `supports_disappearing_timers` carries the target
  chat's timer, looked up under the chat id and then under every address
  linked to it (newest setting timestamp first). A dispatch that waits keeps
  the value it was routed with.

### Notes and info cards

The core turns what it learns about a chat into **system notes** in the
chat's mirror threads (`Action::Note`, sent as a `MessageCreateDispatch` with
`system_note`). The wording lives in one place, `wuphf-core/src/notes.rs`.

- **Baseline, then changes.** The first time a value is seen it is stored
  silently as the baseline. Only a later change of a known value is a change.
  A snapshot report (`ChatUpdateEvent.snapshot`: a history sync, a first
  sighting) is stored without notes even when it differs, so re-pairing does
  not post a note for every chat that drifted while the bridge was down.
- **What becomes a note.** A changed description, picture ("🖼️ Photo changed"
  with the photo attached, or "Photo removed"), disappearing timer (also when
  a message reveals it), and each membership event ("➕ Alice added Bob",
  "➖ Bob left", "⭐ Bob is now an admin", …, the plain wording when no actor is
  known or the member acted on themself). A title change is never a note: it
  renames the mirror thread through the retitle path, snapshots included.
  The member count only changes the stored count.
- **Where notes go.** To every other endpoint of each conversation the chat
  belongs to (under any of its address forms), skipping a mirror endpoint
  whose thread does not exist yet: its info card will show the current state.
- **Notes are not messages.** Acknowledging a note records no copy and sends
  no delivery receipt, so a reply to a note links to nothing, and a note is
  never bridged back. Every current connector drops its own sends before they
  could come back as inbound messages.
- **The info card provisions the topic.** When routing would provision a
  mirror thread for a conversation that has never had one, the core marks the
  conversation `introduced`, sends one info card (title, description, timer,
  member count, with the picture attached) as the provisioning dispatch, and
  holds the message in the durable provisioning stash. The card's
  acknowledgement establishes the thread and the stash replays into it. A
  card that fails is not retried: the stash replays and the message
  provisions the thread itself. Marking the conversation before the card is
  sent makes a second card impossible, even after a crash or a lost thread.
  A replay lets only one held message provision; the rest wait behind it.
- **Pictures.** The connector uploads the picture to the media store and
  reports its id; the core hashes the bytes and stores nothing new when the
  hash is unchanged, so the same picture is never posted twice. Repeated
  uploads of an unchanged picture are orphans and are pruned after
  `orphan_media_retention_seconds`; pruning keeps the media a profile still
  names.

### Chat state

Pin, mute and archive sync both ways between a source chat and its mirror
topics (`wuphf-core/src/chat_state.rs`).

- **One state per conversation**, kept in the source chat's profile under the
  conversation's own source chat id. A mirror topic has no profile of its own.
- **An echo is an equal value.** A report equal to the stored state is a
  no-op, which is what ends the echo of every dispatch the core sent. There is
  no suppression window, so both sides must encode each value exactly (mute
  ends: WhatsApp milliseconds ÷ 1000; WhatsApp "forever" and Telegram's
  `2147483647` are -1).
- **Directions** are named from the source: `chat_state.pin`, `.mute` and
  `.archive` are each `off`, `to_mirror`, `to_source` or `both` (default),
  with per-conversation overrides in `chat_state.chats.<name> = { chat, … }`
  naming the source chat or the mirror topic. A source-side report is always
  stored (the profile mirrors the source) and goes to each mirror topic that
  exists when its direction allows `to_mirror`; a mirror-side report is stored
  and goes to the source only when its direction allows `to_source`,
  otherwise it is dropped.
- **Snapshots** (history syncs, full app-state syncs) set the baseline and
  dispatch nothing. Existing topics are not aligned at startup: the first
  real change on either side syncs.
- **Telegram mapping:** pin ↔ pinned forum topic; mute ↔ the topic's notify
  settings (user sessions only: a bot's notify settings mean nothing, so a bot
  neither reports nor applies mutes); archive ↔ closed topic (the owner, an
  admin, can still post in it). Plain Telegram chats are out of scope.
- **Archiving on WhatsApp also unpins**; that unpin is reported like any
  change, so the topic is unpinned too.

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
configured under `notify` through the ordinary raw-send path, so a notice is
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

The unknown-timer notice (`NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN`: a message
went to a chat whose timer the connector could not learn) is not a health
state: it bypasses that suppression and leaves the connector's health state
alone, and the connector itself limits it to once per chat until the timer is
known. The media notice (`NOTICE_KIND_MEDIA_UNAVAILABLE`: an inbound message
was dropped because its media could not be downloaded) bypasses it the same
way, once per dropped message.
The link notice (`NOTICE_KIND_LINK_REQUIRED`: a WhatsApp connector is waiting
to be linked again, with the QR code attached as `media_ids` or a pair code in
the detail) bypasses it too: every pairing round the connector reports is
news, and the connector spaces them out itself.

## Media

The runtime exposes a bounded media broker. A source connector uploads bytes
once; normalized attachments and dispatches refer only to a core-owned
`MediaId`. The destination connector downloads by id while executing its
dispatch, so platform-private file handles never become cross-platform message
identity. Uploads not linked into a logical message or named by a chat
profile's avatar are pruned after `orphan_media_retention_seconds`; linked
media is retained. The Go connectors upload through
`coreclient.MediaClient`, the Rust ones through `wuphf_connector::MediaClient`.

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

The Instagram connector can also serve `instagram.fetch_media@1` (enable its
`[functions]` section): it downloads a post's or reel's media by link on the
connector's logged-in session and returns them as media ids.

## Feature clients (wuphf-misc)

Features that are not bridging (the first: "when someone posts an Instagram
link in this chat, post the video there") belong neither in the core nor in a
connector. `wuphf-misc` (`misc/`) hosts them as a separate process, built only
on the core's client services, like the TUI: it follows `SubscribeMessages`,
calls functions through `FunctionTransport`, and replies with `SendMessage`.
A reply is an ordinary owner send, so it records its sent copy and fans out to
the chat's mirrors like any message the owner types.

An owner send can name the client that sent it (`SendMessageRequest.client`).
The core keeps it on the outgoing observed row as `sent_by`, and the row keeps
it when the platform reports the send back. The TUI sends as `wuphf-tui` and
wuphf-misc as `wuphf-misc`. wuphf-misc reacts to incoming messages and to
outgoing ones from other clients, but never to a row whose `sent_by` is its
own: this is what keeps a reply whose caption holds a link from triggering
another fetch. It also skips an incoming message in the same chat with the
same text as a reply it sent in the last two minutes, which covers an echo the
platform reports before the core has the acknowledgement.

wuphf-misc authenticates with the observer token, which reads every captured
chat and sends as the owner into any of them. Scoped tokens are planned.

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
