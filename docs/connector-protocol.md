# Connector Protocol

## Session Boundary

A connector opens one session for its platform integration and registers:

- stable connector ID
- platform
- owned endpoint IDs

One session may own multiple endpoints for one platform, including Telegram
topic endpoints. Multiple sessions per platform are deferred.

Registration must match core configuration: connector ID, platform, and owned
endpoint IDs. A connector cannot claim endpoints not assigned to it.

## Connector Input

Connectors send:

- `Event(NormalizedEvent)` for platform-origin input.
- `Acknowledgement(DispatchAcknowledgement)` after successfully creating a
  platform message copy.

The runtime rejects events from endpoints outside the registered session and
acknowledgements for dispatches not assigned to that session.

## Connector Output

The core sends:

- `Dispatch(StoredDispatch)` for pending platform work.
- `Rejected { reason }` when connector input violates session ownership.

`StoredDispatch.id` must be echoed in acknowledgements. The connector supplies
only the resulting native platform message ID; logical identity remains owned
by the core.

Dispatches remain pending until acknowledgement. If a connector reconnects
before acknowledging assigned work, the hub redelivers that work with the
same dispatch ID.

## Transport

Canonical network contract is `crates/wuphf-proto/connector.proto`. It
exposes a bidirectional gRPC `OpenSession` stream plus the `MediaTransport`
service. First session client frame must be `Registration`; following frames
carry message creates or acknowledgements. Server frames carry dispatches or
rejection results.

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

The executable server loads configured endpoint and connector assignments from
TOML:

```sh
cargo run -p wuphf-cli -- serve --config config/example.toml
```

The Telegram connector under `connectors/telegram` is a Go `gotd/td` client.
It registers as `telegram-main`, resolves configured Telegram peer targets,
sends text/image/file dispatches and replies, reports native message IDs
through acknowledgements, and emits inbound text/image/file events only for
explicitly mapped MTProto peer/topic targets. Logical identity, media
identity, and reply resolution remain core-owned.

The WhatsApp connector under `connectors/whatsapp` is a Go `whatsmeow` client.
It persists linked-device session state in its own SQLite database, prints a
QR code for first pairing, resolves configured chat JIDs, and sends/emits
text/image/file messages. Inbound WhatsApp replies are normalized for routing
to platforms already supporting reply dispatch. Outbound WhatsApp quoted
replies are deferred until dispatch includes required quote sender/content
context. Group participant metadata is normalized for core storage. Bridge
echo suppression remains a core decision: WhatsApp messages sent from linked
account are still submitted to core.
