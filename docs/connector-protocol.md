# Connector Protocol

The canonical contract is `crates/wuphf-proto/connector.proto`: a bidirectional
gRPC `OpenSession` stream plus a `MediaTransport` service. Remote functions
(see below) have services of their own.

## Session

A connector opens one session and its first frame must be a `Registration`
(connector id, platform, capabilities). One session is one connector for one
platform. The connector owns no routing or mapping — it reports conversations by
their native address and executes dispatches addressed the same way. The
connector id is stamped by the runtime from the session, never trusted from the
wire.

Registration must match core configuration. Declared capabilities
(`supports_threads`, `supports_topic_create`) gate whether the connector may
host a threaded mirror.

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
  its own outage.

## Server frames (core → connector)

- `MessageCreateDispatch` — addressed by `target_chat_id`/`target_thread_id`,
  with reply context and attachments. If a threaded mirror has no thread yet it
  carries `provision_thread { container, title }`; the connector creates the
  thread, sends, and reports the new thread id in the acknowledgement. Carries a
  `delivery { attachments_as_files, max_message_length }` block resolved
  server-side from the target connector's delivery options: when
  `attachments_as_files` is set the connector sends every attachment as a generic
  file, and when the text exceeds `max_message_length` it sends the text as a
  plain-text file instead of an inline message.
- `MessageEditDispatch` / `MessageDeleteDispatch` — the target copy's native id
  plus an `edit_mode` / `delete_mode` resolved server-side from the target
  connector's delivery options, so the connector renders the operation without
  any local policy.
- `MessageRetitleDispatch` — `target_chat_id`/`target_thread_id` plus the new
  `title`, sent when a source conversation is renamed so the mirror thread's
  title tracks it. Carries no message; the connector renames the thread and
  acknowledges with the thread id.
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
  submitted so the core decides echo suppression.
