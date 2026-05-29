# Telegram Connector

This adapter is implemented in Go using [`gotd/td`](https://github.com/gotd/td)
and connects to the Rust core through the canonical bidirectional gRPC stream.

Implemented slice:

- authenticated Telegram MTProto session with file-backed `gotd/td` session
  storage
- connector registration against the Rust core
- outbound text dispatch to configured Telegram peers/topics
- replies using the Telegram-native parent message ID resolved by the core
- acknowledgement of the resulting Telegram message identifier
- inbound text/image/file update normalization for configured MTProto
  peers/topics
- core media broker upload/download for one image or file attachment per
  message

Deferred connector work:

- video, audio, sticker, and multi-attachment handling
- edits, deletes, and reactions

Routing, replication membership, deduplication, sent-message loop prevention,
and logical identity remain in the Rust core. The connector only executes core
dispatches and reports Telegram-native message identifiers.

Each target includes exact incoming MTProto peer identity and replication
group ID. Updates outside configured peers/topics are ignored. Media-bearing
updates outside the initial image/file support are ignored. Media bytes are
uploaded to and fetched from the Rust core by internal media ID; Telegram file
locations remain connector-local.

Target mappings must be unambiguous: one outbound target per endpoint, and
either one whole-peer inbound target or distinct topic targets for a peer.
Connector input transport carries protobuf client frames so future event
normalizers can be added without changing session plumbing.

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
