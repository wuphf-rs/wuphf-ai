# WhatsApp Connector

This adapter uses [`whatsmeow`](https://github.com/tulir/whatsmeow) and the
shared gRPC contract under `proto/go`.

Implemented slice:

- linked-device authentication by QR code or pair code (`login_method`), at
  the first run and again by itself after a logout, with the code also sent to
  the owner through the core's notice chats
- connector-private SQLite session storage
- raw-pipe inbound/outbound text, image, and file replication keyed by
  WhatsApp JID chat IDs
- inbound reply normalization through native WhatsApp stanza IDs
- WhatsApp group participant normalization
- phone-to-LID identity migration reported to the core so both addresses
  resolve to one conversation
- media bytes transferred through core-owned `MediaId` broker
- disappearing-message timers: each chat's timer is reported to the core
  (from messages, timer changes, group notifications and history syncs) and
  stamped on every outgoing part, so bridged replies disappear with the chat;
  a group's unknown timer is looked up, and a DM's raises one owner notice.
  Needs `supports_disappearing_timers = true` here and in the core's
  `[[connectors]]` entry
- chat profiles: group renames, descriptions and membership changes (with
  who made them), picture changes (the picture is uploaded to the core),
  contact and push-name changes and history-sync titles are reported to the
  core, which posts them as notes in the mirror topic; the first message of a
  chat in each run reports the chat's current state first, for the topic's
  info card. @mentions show the contact's name instead of the number

Deferred connector work:

- video, audio, sticker, and multi-attachment handling
- edits, deletes, and reactions
- end-to-end live integration tests

Routing, identity, deduplication, echo suppression, reply mapping, and media
retention remain in Rust core. The connector is a raw pipe: it emits WhatsApp
events tagged with their native chat JID and executes dispatches addressed by
chat JID, owning no mapping itself. WhatsApp has no threads, so this connector
declares no thread capabilities and is never used as a threaded mirror. It
intentionally submits messages authored by the linked account; core decides
whether each is user input or a bridge echo.

## Configuration

Copy `config.example.toml`. The connector owns no routing; configure which
conversations map where in the core's `[rules.<id>]` entries using WhatsApp
JIDs as chat IDs, for example a direct chat `12345@s.whatsapp.net` or group
`123456789@g.us`.

The connector authenticates to the core with the token in the variable
`core_token_env` names, which must hold the same value the core reads from this
connector's `[[connectors]].token_env`.

Run core server, then connector:

```sh
export WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN=replace-me
go run ./cmd/wuphf-whatsapp --config config.example.toml
```

First run links the session: by default it prints a QR code to scan in
WhatsApp → Linked devices; with `login_method = "pair_code"` and
`phone_number` set it asks for a pair code to type in after "Link with phone
number instead". Either way the code also reaches the owner as a link-required
notice (the QR code as a picture). When WhatsApp logs the device out the
connector links a new one the same way, without a restart; messages for
WhatsApp wait in the core meanwhile. The session is stored at the configured
`session_database` path.

The keys marked "Runtime." in `config.example.toml` can also change while the
connector runs, from the core's settings panel or `wuphf settings`: the
connector declares them to the core and applies each change the core pushes
(see `docs/connector-protocol.md`, "A connector's own settings").

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
