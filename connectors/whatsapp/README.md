# WhatsApp Connector

This adapter uses [`whatsmeow`](https://github.com/tulir/whatsmeow) and the
shared gRPC contract under `proto/go`.

Implemented slice:

- linked-device authentication with terminal QR pairing on first run
- connector-private SQLite session storage
- raw-pipe inbound/outbound text, image, and file replication keyed by
  WhatsApp JID chat IDs
- inbound reply normalization through native WhatsApp stanza IDs
- WhatsApp group participant normalization
- phone-to-LID identity migration reported to the core so both addresses
  resolve to one conversation
- media bytes transferred through core-owned `MediaId` broker

Deferred connector work:

- video, audio, sticker, and multi-attachment handling
- edits, deletes, and reactions
- end-to-end live integration tests

Routing, identity, deduplication, echo suppression, reply mapping, and media
retention remain in Rust core. The connector is a raw pipe: it emits WhatsApp
events tagged with their native chat JID and executes dispatches addressed by
chat JID, owning no mapping itself. WhatsApp has no threads, so this connector
declares no capabilities and is never used as a threaded mirror. It
intentionally submits messages authored by the linked account; core decides
whether each is user input or a bridge echo.

## Configuration

Copy `config.example.toml`. The connector owns no routing; configure which
conversations map where in the core's `[[rules]]` using WhatsApp JIDs as chat
IDs, for example a direct chat `12345@s.whatsapp.net` or group
`123456789@g.us`.

Run core server, then connector:

```sh
go run ./cmd/wuphf-whatsapp --config config.example.toml
```

First run prints QR code; scan using WhatsApp linked-device flow. Session is
stored at configured `session_database` path.
