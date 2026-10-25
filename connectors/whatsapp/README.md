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

The connector authenticates to the core with the token in the variable
`core_token_env` names, which must hold the same value the core reads from this
connector's `[[connectors]].token_env`.

Run core server, then connector:

```sh
export WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN=replace-me
go run ./cmd/wuphf-whatsapp --config config.example.toml
```

First run prints QR code; scan using WhatsApp linked-device flow. Session is
stored at configured `session_database` path.

## Animated stickers

WhatsApp stickers are WebP, so the connector converts animated stickers from
other platforms (Telegram WebM video stickers and Lottie `.tgs` stickers) to
animated WebP before sending them. It does not convert them itself: it calls
the core's `media.convert@1` function, which the `providers/mediaconv` process
serves (that process needs `ffmpeg`, ImageMagick and the rlottie renderer; this
connector needs none of them). When the conversion fails or no converter is
available, the sticker is sent as a file.

Delivery on a connector is sequential, so a conversion holds back the messages
queued behind it. If the core's `[functions]` configuration allows a
`media.convert` provider that is not running, every animated sticker waits up
to `media_convert_timeout_ms` (default 20 s) before falling back. If you do not
run the provider, remove its `[[functions.providers]]` entry from the core
configuration: calls then fail at once and the sticker falls back immediately.

When the core sets `invoke_token_env`, set `functions_token_env` here to the
name of an environment variable holding the same token.
