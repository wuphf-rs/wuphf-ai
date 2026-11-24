# Connector Protocol

The canonical contract is `crates/wuphf-proto/connector.proto`: a bidirectional
gRPC `OpenSession` stream plus a `MediaTransport` service. Remote functions
(see below) have services of their own.

## Session

A connector opens one session and its first frame must be a `Registration`
(connector id, platform, capabilities, and optionally its own settings; see
"A connector's own settings"). One session is one connector for one
platform. The connector owns no routing or mapping — it reports conversations by
their native address and executes dispatches addressed the same way. The
connector id is stamped by the runtime from the session, never trusted from the
wire.

Registration must match core configuration, capabilities included: a connector
whose registration differs from its `[[connectors]]` entry is refused.
Declared capabilities (`supports_threads`, `supports_topic_create`) gate
whether the connector may host a threaded mirror. `supports_disappearing_timers`
declares that the connector stamps a chat's disappearing-messages timer on what
it sends and reports the timers it observes; the core fills
`MessageCreateDispatch.disappearing` only for connectors that declare it. Set
it on both sides (the core's entry and the connector's own config) together.

`OpenSession` requires the connector's token as `authorization: Bearer <token>`.
The core reads each connector's token from the environment variable its
`[[connectors]]` entry names in `token_env`, and the token must belong to the
connector id in the registration frame: a missing token, or another
connector's, is refused with `UNAUTHENTICATED` before the session reaches the
hub. A second session for a connector whose session is still live is refused
with `ALREADY_EXISTS`; the live session keeps its dispatches. A connector that
restarts gets in as soon as its old stream has ended, which the core notices at
once when the old process exits and within the HTTP/2 keepalive window when the
connection dies silently; the SDKs keep retrying with backoff until then.

## Client frames (connector → core)

- `MessageCreateEvent` — raw `chat_id`, optional `thread_id`/`chat_title`,
  `platform_message_id`, `sender`, optional `participant`/`sender_name`,
  text/attachments, optional native `reply_to_platform_message_id`.
  `disappearing` is the chat's timer as the message carried it (WhatsApp:
  `ContextInfo.expiration`, the owner's own messages included), set only when
  greater than 0. A message without one never turns a known timer off; when
  the chat's timer is still unknown and the connector declares
  `supports_disappearing_timers`, the core records it as off. The core records
  the timer before routing, so chats no rule matches count too.
- `ChatUpdateEvent` — a batch of `ChatUpdate`s, each a chat's properties as
  the platform reported them (a timer setting message, a group notification, a
  picture or contact change, a history sync, a first sighting). `chat_id`s are
  canonical, exactly as the connector reports them in `MessageCreateEvent`; an
  absent field leaves what the core knows unchanged.
  - `disappearing`: with a setting timestamp on both the stored and the
    reported timer the newer wins; otherwise the later report wins.
  - `title` is the chat's **display title**, exactly the string the connector
    sends as `MessageCreateEvent.chat_title` (tags included, e.g. `[G] …`). A
    change renames the mirror thread; it is never a note.
  - `description`: an empty string means the description was cleared.
  - `avatar { id, media_id }`: `id` is the platform's picture id, empty when the
    picture was removed. `media_id` names the picture the connector uploaded to
    the core's media store first; leave the whole `avatar` out when the picture
    could not be fetched. The core compares pictures by a hash of their bytes,
    so re-uploading the same picture changes nothing.
  - `participant_count`: the member count, when the connector read it.
  - `participant_changes`: membership events in order — `kind` (joined, left,
    promoted, demoted), the member's canonical `participant` id and display
    `name`, and, when the platform says who made the change, its `actor` (in
    the same address form as `participant`) and `actor_name`. They are events:
    never part of a snapshot.
  - `thread_id`: set when the update is about a thread (a Telegram forum
    topic) rather than the whole chat. The core resolves the chat and thread to
    the conversation's mirror endpoint. Such an update carries chat state only.
  - `pinned`, `mute`, `archived`: the chat's (or topic's) state. `mute` is
    `MuteState { muted_until }`: 0 not muted, -1 muted forever, otherwise the
    end in Unix **seconds**; a mute whose end has passed is reported as 0. A
    Telegram topic reports a closed topic as `archived`. Encode each value
    exactly and reversibly: the core recognizes an echo of its own dispatch
    only because the value it gets back equals the value it sent.
  - `snapshot` (on the event): the updates describe current state (a history
    sync, the first time this connector process sees a chat, a full app-state
    sync), not live changes. The core stores them without posting notes or
    dispatching chat state.
- `MessageEditEvent` / `MessageDeleteEvent` — the original message's native id
  plus the change; the core resolves it to the logical message and fans it out.
- `Acknowledgement` — echoes the `dispatch_id`, supplies the resulting native
  `platform_message_id`, and sets `thread_id` only when the dispatch provisioned
  a new thread.
- `IdentityMigrated` — old/new chat (and optional thread) ids when a platform
  changes a live conversation's address (e.g. WhatsApp phone-to-LID). The new
  address becomes canonical; both stay resolvable.
- `DispatchFailed` — a dispatch that can never succeed (e.g. invalid peer), so
  the core dead-letters it instead of redelivering. Transient failures are not
  reported this way: the connector simply drops its session and the core
  redelivers.
- `OwnerNotice` — the connector's own health: a `NoticeKind` (logged out, auth
  required, degraded, recovered) plus optional platform detail. Report what
  happened, not how it should read — the core composes the message, picks the
  chats, and drops a repeat of a state it has already reported. It is relayed
  only through *other* connectors, so a connector cannot be asked to announce
  its own outage. `NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN` is not about health: a
  message went to a chat whose timer the connector could not learn, so it will
  not disappear (`detail` names the chat). The core relays it without folding it
  into the connector's health state; the connector limits repeats (once per chat
  until the timer becomes known). `NOTICE_KIND_MEDIA_UNAVAILABLE` is not about
  health either: an inbound message was not bridged because its media could not
  be downloaded (`detail` names the media kind, the sender, the chat and the
  error). It is relayed the same way, once per dropped message.
  `NOTICE_KIND_LINK_REQUIRED` is not about health: the connector is waiting to
  be linked to its account again (a WhatsApp pairing, at the first start or
  after a logout). `detail` says how (scan a QR code, or enter the pair code it
  names) and for how long; `media_ids` may carry the QR code, uploaded to the
  core's media store first. The core attaches the media to every message of
  the notice, leaving out an id the store does not hold; the connector limits
  repeats (the first code at once, later ones at growing gaps).

## Server frames (core → connector)

- `MessageCreateDispatch` — addressed by `target_chat_id`/`target_thread_id`,
  with reply context and attachments. If a threaded mirror has no thread yet it
  carries `provision_thread { container, title }`; the connector creates the
  thread, sends, and reports the new thread id in the acknowledgement. How the
  copy is rendered (attachments as files, text spillover) is the connector's
  own configuration; the dispatch carries content only. `disappearing` is the
  target chat's timer as the core knew it when the send was routed: absent when
  never learned, `seconds` 0 when known to be off, and set only for connectors
  that declare `supports_disappearing_timers`. The connector puts it on every
  part of a content send; edits, deletes and reactions carry none.
  `system_note` marks a note the core composed about the chat (an info card, a
  profile change): render it as plain text with its attachments, with no sender
  label and replying to nothing (a thread's root is fine). The core records no
  copy for it, so nothing can reply to it or bridge it back; a connector that
  cannot send its attachments sends the text and acknowledges.
- `MessageEditDispatch` / `MessageDeleteDispatch` — the target copy's native id
  plus an `edit_mode` / `delete_mode` resolved server-side from the target
  connector's delivery options, so the connector renders the operation without
  any local policy.
- `MessageRetitleDispatch` — `target_chat_id`/`target_thread_id` plus the new
  `title`, sent when a source conversation is renamed so the mirror thread's
  title tracks it. Carries no message; the connector renames the thread and
  acknowledges with the thread id.
- `ChatStateDispatch` — bring the target chat (or thread) to this `pinned` /
  `mute` / `archived` state; absent fields are left alone. Acknowledge with the
  target chat id. A field the connector cannot apply (a mute on a Telegram bot
  session, a plain Telegram chat without a thread) is acknowledged without
  acting; a connector that does not implement the dispatch at all acknowledges
  it unchanged (both SDKs do this by default). Applying it sends no frame: the
  platform's own echo comes back through the normal event path.
- `Rejected` — the connector's last input violated session ownership.

Dispatches remain pending until acknowledged with the same `dispatch_id`; a
reconnecting connector is redelivered its outstanding work.

## Media

Attachments carry an internal `media_id` plus type, filename, MIME type, and
size. A source connector streams bytes to `MediaTransport` before submitting the
event; the destination downloads by `media_id` while executing its dispatch.
Uploads are bounded by `max_media_bytes`, persisted in the core so retries and
restarts don't lose referenced content, and pruned when unreferenced. Both
`UploadMedia` and `DownloadMedia` require a bearer token: any configured
connector's token or any function provider's token (providers such as
`mediaconv` move media too). A call without a known token is refused with
`UNAUTHENTICATED`. The core rejects events whose attachment metadata doesn't
match the broker-issued record.

## Remote functions

`crates/wuphf-proto/functions.proto` defines two more services, separate from
the connector session. Provider processes serve named functions over
`ProviderTransport`; any client (a connector, the CLI, another provider) calls
them through `FunctionTransport`. The core only brokers calls and never calls a
function itself.

**Names.** A function is a lowercase dotted name (`media.convert`, at least two
segments of `[a-z][a-z0-9_]*`) plus a major version of at least 1. A breaking
change to the body format is a new major; `(name, major)` is the key.

**Providers.** A provider opens `OpenProviderSession` with
`authorization: Bearer <token>`, where the token is its own, read by the core
from the `token_env` variable of its `[[functions.providers]]` entry. Its first
frame is a `ProviderHello`: its provider id and a `FunctionDeclaration` per
function, giving request and response JSON Schemas (listed, not enforced),
whether the function is `idempotent`, its `max_concurrency` (0 means 1, at most
1024) and an optional suggested deadline. The core rejects the whole hello if
any declaration is invalid (`INVALID_ARGUMENT`), names a function outside the
provider's configured patterns (`PERMISSION_DENIED`), or names a function another
provider id serves (`ALREADY_EXISTS`). A second session under the same provider
id takes over: the old session's stream ends and its in-flight calls count as
lost. The core then streams `ProviderCall` frames (call id, function, args,
media ids, a Unix-millisecond deadline, the caller's label) and `CancelCall`
frames; the provider answers each call with a `CallResult` carrying either a
`CallSuccess` (JSON result and media ids) or a `CallError` (`INVALID_ARGUMENT`,
`UNSUPPORTED` or `FAILED`). A result for a cancelled call is ignored.

**Callers.** `Invoke` takes the function, JSON args, media ids and a free-form
caller label. Args and results are capped at `max_body_bytes`; bulky data is
uploaded through `MediaTransport` and passed as media ids. The call's deadline
is the caller's gRPC deadline capped by `max_deadline_ms`, or the default when
the caller sets none (the function's override, else the provider's suggestion,
else `default_deadline_ms`). Time spent waiting for a provider counts against
it, and a caller that cancels or runs out of time cancels the call at the
provider. When `invoke_token_env` is set, callers present that shared token as
`authorization: Bearer <token>`. `ListFunctions` reports each function's
provider, declaration, load and completed/failed/abandoned counters;
`cargo run -p wuphf-cli -- functions list` prints it.

Status codes a caller can see:

| Code | Meaning |
|------|---------|
| `NOT_FOUND` | No configured provider may serve the name; returned at once. |
| `RESOURCE_EXHAUSTED` | `max_queued_calls` calls are already waiting for the function. |
| `DEADLINE_EXCEEDED` | No provider connected, the call stayed queued behind busy providers, or the provider did not answer in time; the message says which. |
| `UNAVAILABLE` | The provider disconnected mid-call and the function is not idempotent. |
| `INVALID_ARGUMENT` | Malformed function reference, args over the cap or not JSON, or the provider rejected the args. |
| `UNIMPLEMENTED` | The provider reported the request as unsupported. |
| `INTERNAL` | The provider failed, or returned a result over the cap or not JSON. |

**Configuration.** The `[functions]` section of the core configuration (all
durations in milliseconds; defaults shown):

```toml
[functions]
default_deadline_ms = 30000
max_deadline_ms = 60000
keepalive_interval_ms = 10000   # HTTP/2 keepalive, server-wide
keepalive_timeout_ms = 20000
max_body_bytes = 1048576
max_queued_calls = 256          # per function name and major
invoke_token_env = "WUPHF_INVOKE_TOKEN"

[[functions.providers]]
id = "mediaconv"
token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
functions = ["media.convert"]   # exact names, "prefix.*" or "*"

# Per-function limits, applying to every major of the name.
[functions.overrides."media.convert"]
default_deadline_ms = 20000
# max_deadline_ms, max_concurrency, max_body_bytes, max_queued_calls
```

The core refuses to start when a token variable is unset or empty, a value is
zero, a default deadline exceeds its maximum, or a provider or pattern is
malformed.

## Observer service

`crates/wuphf-proto/observer.proto`'s `ObserverTransport` is the client
surface (the TUI, wuphf-misc): `ListChats`, `GetMessages` and
`SubscribeMessages` read the observation log, and `SendMessage` sends as the
owner. Every call needs `authorization: Bearer <observer_token>` when the core
sets one.

`SendMessage` takes the chat (`connector_id`, `chat_id`, optional
`thread_id`), `text`, an optional `reply_to_platform_message_id`, and:

- `media_ids`: attachments, as ids of media already in the core (uploaded
  through `MediaTransport`, or returned by a function). The core sends each
  with its stored metadata. `text` may be empty when `media_ids` is not;
  neither, an id that is not a UUID, or an id of no stored media is
  `INVALID_ARGUMENT`.
- `client`: names the sender (`wuphf-tui`, `wuphf-misc`). The core stores it
  on the outgoing `ObservedMessage` as `sent_by`, which the platform's echo of
  the send keeps. Incoming rows and sends without a client have no `sent_by`.

The reply carries the native id once the connector acknowledges the send
within the core's wait window, else an empty id (the send still proceeds).

## Settings

The core serves its runtime settings as `botconf.v1.Settings` (botconf's
`proto/botconf/v1/settings.proto`) on the same address. A connector that
offers a settings panel (for example a `/wuphf` command) is a client of it:

- Send the connector's own token, `authorization: Bearer <token>`, the same
  one as for the connector session.
- Set `actor` on every request to the platform user who acted, with `id` as
  the connector reports actors (e.g. `704338780` for a Telegram user). The
  core allows it only when that id is in the connector's `owner_ids` in the
  core's config.
- `UNAUTHENTICATED`: the token is missing or unknown. `PERMISSION_DENIED`: no
  actor, or the actor is not one of this connector's owners. Show the user a
  short "not allowed" and change nothing.
- Never mirror a settings command or a panel answer as a chat message.

The panel's screens come from the core (`Render`, `Act`); the connector only
draws them and sends back what was pressed or typed.

### A connector's own settings

A connector may declare its own runtime settings in its `Registration`, in
the optional `settings` field (`ConnectorSettingsDeclaration`):

- `descriptor`: a `botconf.v1.DescribeResponse`, serialized with protobuf,
  listing the settings (its `sections` is ignored). Keys are relative to the
  connector's section, e.g. `rendering.edit_mode`.
- `json_schema`: a JSON Schema (draft 2020-12) of the whole section.
- `base_json`: the connector's own file values for the section, as a JSON
  object.

The core keeps the section at `connector_settings.<connector id>`: it stores
and validates every change with the schema (a panel's text answers are read as
the booleans and numbers the schema asks for), and the connector's file values
stay the base, so unsetting an override returns to them. The core's own
configuration may not hold `[connector_settings]`.

- **Live-safe keys only.** A runtime setting applies at once, so only
  `rendering.*`, `sticker_metadata.*`, `log_level` and
  `media_convert_timeout_ms` may be declared. Identity, addresses, session
  files, `client_mode` and capabilities stay in the connector's file. A
  declaration with any other key, or a schema that does not compile, refuses
  the session with `INVALID_ARGUMENT` before it reaches the hub.
- **Kept across restarts.** The core stores each connector's last
  declaration and registers its section at startup, so overrides apply (and
  can be edited, e.g. with `wuphf settings`) while the connector is down. A
  registration without a declaration keeps the section declared before.
- **Following them.** The connector watches its own section through the
  settings service (`Watch` with the prefix `connector_settings.<id>`), with
  its own token and no actor: that is the one thing a connector may do
  without someone acting. Each event's values are laid over the base (an
  `Initial` event starts again from it). The Go connectors use
  `connectors/shared/settings` for all of this, reading the settings once per
  dispatch.

### A connector's commands

A connector may also declare commands, things the owner can run on it that are
not settings (a resync, say): `Registration.commands`, each a
`ConnectorCommand { id, title }`. While the session is live, the core's
settings panel shows them as buttons on the connector's section (`title` is the
label). A press is sent on the session as `ServerFrame.run_command`
(`RunCommand { request_id, command_id }`); the connector answers with
`ClientFrame.command_result` (`CommandResult { request_id, ok, message }`),
whose `message` the panel shows (as an error when not `ok`). The core waits
10 s; answer at once and do slow work in the background.

Commands are requests, not dispatches: nothing is stored or redelivered, and
a command for a connector that is not connected is refused. The core decides
nothing about them beyond offering what the live session declared; a
`RunCommand` for an id the connector did not declare is answered not `ok`.
The SDKs take a commands handler beside the sender (`Options.commands` in
Rust, `Options.Commands` in Go).

### A plugin's own settings

A plugin (wuphf-misc: a client of the observer and function services, neither
a connector nor a provider) declares its settings with one call,
`PluginTransport.DeclareSettings` (`plugin.proto`), authenticated with the
observer token and refused (`UNAUTHENTICATED`) when the core has none. The
request carries a `plugin_id` (lowercase letters, digits, `-` and `_`) and the
same `ConnectorSettingsDeclaration` a connector sends. The core keeps the
section at `plugin_settings.<plugin id>`, and the plugin follows it with
`Watch` on that prefix, also with the observer token.

- **Any key but the connection's.** The core does not know a plugin's
  features, so it does not limit the keys as it does for connectors; only
  `core_address`, `plugin_id`, `token_env` and `*_token_env` (at any depth)
  are refused, with `INVALID_ARGUMENT`.
- **Kept across restarts**, as a connector's: every stored plugin declaration
  is registered at startup. The core's configuration may not hold
  `[plugin_settings]`.
- Go plugins use `settings.ConnectPlugin` in `connectors/shared/settings`.

## Health probes and tools

The core checks that its rules can deliver (plan 079). It asks a live
session about the chats the rules use with `ServerFrame.probe`
(`ProbeChats { request_id, chat_ids }`), and the connector answers with
`ClientFrame.probe_result` (`ProbeResult { request_id, chats, tools }`):

- One `ChatFacts` per chat it can say something about: `exists`, `member`,
  `can_send`, `forum`, `can_create_topics`, `can_pin`, `can_delete_others`,
  each a `Fact` (`FACT_UNKNOWN`, `FACT_YES`, `FACT_NO`), and `detail`, the
  platform's own words for a `FACT_NO` or a failed lookup. A connector only
  reports what it sees. It never decides what a rule needs.
- Answer within 20 s; a late or missing answer is unknown, and so is a
  fact left `FACT_UNKNOWN` or a chat left out. Unknown never changes what
  the core decided before.
- Answer in the background, like a command: a probe is not a dispatch, so
  nothing is stored or redelivered.

`tools` (in `Registration.tools`, `ProviderHello.tools` and every
`ProbeResult`) lists the external programs a process runs (`ToolStatus {
name, available, needed_for }`). The core warns about a missing one. A
provider leaves out a function it cannot run, and a call to a function its
connected provider withheld fails at once (`UNAVAILABLE`) instead of waiting.

The SDKs take a prober beside the sender (`Options.prober` in Rust,
`Options.Prober` and `Options.Tools` in Go, with `coreclient.ToolStatus`
for a `PATH` lookup).

## Connectors

- **Telegram** (`connectors/telegram`, `gotd/td`) — registers with forum-topic
  capabilities, emits typed chat ids (`telegram:channel|chat|user:N`) via an
  access-hash peer cache, provisions forum topics on demand, renames them on a
  source rename, and renders replicated edits/deletes per the dispatch-supplied
  mode.
- **WhatsApp** (`connectors/whatsapp`, `whatsmeow`) — QR-paired with its own
  SQLite session, keyed by JID chat ids. Declares no thread capabilities (never
  a mirror), reports phone-to-LID migrations, syncs newsletter/contact names,
  and builds real quoted replies. Linked-account messages are still submitted so
  the core decides echo suppression.
- **Instagram** (`connectors/instagram`, `mautrix/meta`) — cookie-authenticated
  via a JSON session file, keyed by numeric thread ids. Declares no thread
  capabilities (never a mirror). Currently an MVP: inbound and outbound text
  only, with native reply ids carried through. Own-account messages are still
  submitted so the core decides echo suppression. With its `[functions]`
  section it also serves `instagram.fetch_media@1`: args `{"url": ...}` (a
  `/p/`, `/reel/` or `/reels/` link), result `{"items": [{"kind":
  "video"|"image"}], "author", "caption", "skipped"}` with the media ids in
  item order; a carousel gives one item per medium, and media over 64 MiB or
  the core's upload limit are left out and counted in `skipped`.
