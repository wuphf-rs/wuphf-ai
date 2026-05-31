# Telegram Connector

This adapter is implemented in Go using [`gotd/td`](https://github.com/gotd/td)
and connects to the Rust core through the canonical bidirectional gRPC stream.

Implemented slice:

- authenticated Telegram MTProto session with file-backed `gotd/td` session
  storage
- connector registration against the Rust core, declaring forum-topic
  capabilities
- raw-pipe inbound text/image/file normalization tagged with typed chat IDs
  (`telegram:channel:N`, `telegram:chat:N`, `telegram:user:N`) and forum topic
  thread IDs
- outbound text dispatch to any chat ID, with on-demand forum topic
  provisioning when the core dispatches to a threaded mirror
- replies using the Telegram-native parent message ID resolved by the core
- acknowledgement of the resulting Telegram message identifier, plus the newly
  provisioned topic ID when a thread was created
- access-hash peer cache populated from update entities so observed peers can
  be addressed without preconfiguration
- core media broker upload/download for one image or file attachment per
  message

Deferred connector work:

- video, audio, sticker, and multi-attachment handling
- topic retitle on conversation rename
- edits, deletes, and reactions

Routing, replication membership, deduplication, sent-message loop prevention,
and logical identity remain in the Rust core. The connector is a raw pipe: it
emits every observed update tagged with its native chat/thread ID and executes
dispatches addressed by chat ID, owning no mapping itself. Media-bearing
updates outside the initial image/file support are ignored. Media bytes are
uploaded to and fetched from the Rust core by internal media ID; Telegram file
locations remain connector-local.

## Configuration

Copy `config.example.toml` and provide Telegram credentials through
environment variables:

```sh
export WUPHF_TELEGRAM_APP_ID=123456
export WUPHF_TELEGRAM_APP_HASH=replace-me
```

Run the core server, then the connector from this directory:

```sh
go run ./cmd/wuphf-telegram --config config.example.toml
```
