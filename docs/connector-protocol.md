# Connector Protocol

The canonical contract is `crates/wuphf-proto/connector.proto`: a bidirectional
gRPC `OpenSession` stream plus a `MediaTransport` service.

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
restarts don't lose referenced content, and pruned when unreferenced. The core
rejects events whose attachment metadata doesn't match the broker-issued record.

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
