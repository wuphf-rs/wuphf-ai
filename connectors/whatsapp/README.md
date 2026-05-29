# WhatsApp Connector

This adapter uses [`whatsmeow`](https://github.com/tulir/whatsmeow) and the
shared gRPC contract under `proto/go`.

Implemented slice:

- linked-device authentication with terminal QR pairing on first run
- connector-private SQLite session storage
- configured direct-chat/group JID mapping
- inbound/outbound text, image, and file replication
- inbound reply normalization through native WhatsApp stanza IDs
- WhatsApp group participant normalization
- media bytes transferred through core-owned `MediaId` broker

Deferred connector work:

- video, audio, sticker, and multi-attachment handling
- outbound quoted replies; core dispatch must first carry required quote
  sender/content metadata
- edits, deletes, and reactions
- reconnect/backoff hardening and end-to-end live integration tests

Routing, identity, deduplication, echo suppression, reply mapping, and media
retention remain in Rust core. Connector normalizes WhatsApp events and
executes dispatches only. It intentionally submits messages authored by the
linked account; core decides whether each is user input or a bridge echo.

## Configuration

Copy `config.example.toml`. `chat_jid` must be exact WhatsApp chat identity
reported by `whatsmeow`, for example a direct chat `12345@s.whatsapp.net` or
group `123456789@g.us`.

Run core server, then connector:

```sh
go run ./cmd/wuphf-whatsapp --config config.example.toml
```

First run prints QR code; scan using WhatsApp linked-device flow. Session is
stored at configured `session_database` path.
