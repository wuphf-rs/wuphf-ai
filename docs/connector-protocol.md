# Connector Protocol

## Session Boundary

A connector opens one session for its platform integration and registers:

- stable connector ID
- platform
- capabilities (`supports_threads`, `supports_topic_create`)

One session represents one connector for one platform. The connector owns no
routing or mapping: it is a raw pipe that reports every observed conversation
by its native address and executes dispatches addressed the same way. All
mapping between conversations lives in the core's rules.

Registration must match core configuration: connector ID and platform. The
declared capabilities gate how the core may use the connector — a connector
without `supports_threads` can never be used as a threaded mirror, and
`supports_topic_create` requires `supports_threads`.

## Connector Input

Connectors send:

- `MessageCreate(MessageCreateEvent)` for platform-origin input. The event
  carries the raw `chat_id`, optional `thread_id`, optional `chat_title`,
  `platform_message_id`, `sender`, optional `participant`, text/attachments,
  and an optional native `reply_to_platform_message_id`. The connector ID is
  stamped by the runtime from the session, never sent on the wire.
- `Acknowledgement(Acknowledgement)` after successfully creating a platform
  message copy. It echoes the `dispatch_id`, supplies the resulting native
  `platform_message_id`, and sets `thread_id` only when the dispatch
  provisioned a new thread.
- `IdentityMigrated(IdentityMigrated)` when a platform changes the address of a
  live conversation (for example WhatsApp's phone-to-LID migration). It reports
  the old and new chat (and optional thread) IDs so the core can link both
  addresses to one conversation. The new address is treated as canonical.

## Connector Output

The core sends:

- `MessageCreate(MessageCreateDispatch)` for pending platform work, addressed by
  `target_chat_id` and optional `target_thread_id`. When the core routes to a
  threaded mirror that has no thread yet, the dispatch carries a
  `provision_thread { container, title }` instruction; the connector creates the
  thread and reports its ID back in the acknowledgement.
- `Rejected { reason }` when connector input violates session ownership.

`MessageCreateDispatch.dispatch_id` must be echoed in acknowledgements. The
connector supplies only the resulting native platform message ID (and a
provisioned thread ID where applicable); logical identity remains owned by the
core.

Dispatches remain pending until acknowledgement. If a connector reconnects
before acknowledging assigned work, the hub redelivers that work with the
same dispatch ID.

## Transport

Canonical network contract is `crates/wuphf-proto/connector.proto`. It
exposes a bidirectional gRPC `OpenSession` stream plus the `MediaTransport`
service. First session client frame must be `Registration`; following frames
carry message creates, acknowledgements, or identity migrations. Server frames
carry dispatches or rejection results.

Attachments carry an internal `media_id`, type, filename, MIME type, and
size. A source connector uploads attachment bytes to `MediaTransport` before
submitting the normalized event. A destination connector downloads those
bytes by `media_id` while executing its dispatch. Platform-specific file
locations and URLs never become cross-platform message identity.
The event attachment metadata must be the metadata returned by upload; the
core rejects missing media IDs or altered metadata before accepting the event.

Media uploads are streamed and bounded by the server `max_media_bytes`
configuration. The initial SQLite implementation persists media bytes in the
core so dispatch retry and connector restart do not lose referenced content.
Uploads not subsequently linked into a logical message are removed after
`orphan_media_retention_seconds` once later upload traffic triggers cleanup.
Media linked to a persisted logical message is retained; deleting that content
requires a future message-retention policy rather than an unsafe age-only
cleanup.

`wuphf-runtime::grpc` translates protobuf frames to internal connector hub.
Routing and identity remain outside transport adapter.

The executable server loads connector declarations and routing rules from TOML:

```sh
cargo run -p wuphf-cli -- serve --config config/example.toml
```

The Telegram connector under `connectors/telegram` is a Go `gotd/td` client.
It registers as `telegram-main` declaring forum-topic capabilities, emits every
observed update tagged with a typed chat ID (`telegram:channel:N`,
`telegram:chat:N`, `telegram:user:N`) and forum topic thread ID, sends
text/image/file dispatches and replies to any addressed chat, provisions forum
topics on demand for threaded mirrors, and reports native message IDs (plus any
provisioned topic ID) through acknowledgements. Logical identity, media
identity, and reply resolution remain core-owned.

The WhatsApp connector under `connectors/whatsapp` is a Go `whatsmeow` client.
It persists linked-device session state in its own SQLite database, prints a
QR code for first pairing, and emits/executes text/image/file messages keyed by
WhatsApp JID chat IDs. WhatsApp has no threads, so it declares no capabilities
and is never used as a threaded mirror. It reports phone-to-LID identity
migrations so both addresses resolve to one conversation, and normalizes group
participant metadata for core storage. Bridge echo suppression remains a core
decision: WhatsApp messages sent from the linked account are still submitted to
the core.
