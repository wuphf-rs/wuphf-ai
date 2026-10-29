# wuphf

`wuphf` is a multi-platform messaging replication engine. It models **one
platform-independent logical message and its copies** at platform endpoints,
rather than forwarding platform messages without identity tracking. A Rust core
owns all identity, routing, dedup, and loop prevention; Go connectors are thin
pipes.

For a human-friendly tour, open [`docs/index.html`](docs/index.html).

## What works

- Rule-based dynamic routing: raw platform addresses resolve to endpoints
  through an alias table; conversations and mirror threads are minted lazily
  and reused.
- Address-form independence: a platform that reaches one chat by several
  addresses (a WhatsApp phone JID and its LID) is treated as one chat. A rule
  can name a chat by any form the platform has reported, in an allowlist or a
  blocklist alike.
- Message **create, edit, and delete** replication. Rendering (in-place vs
  repost edits; revoke vs tombstone deletes; attachments-as-files; over-length
  text spillover) is each connector's own `[rendering]` config. Internal reply
  identity is resolved to native ids per target.
- Durable dispatch state (pending / blocked / acknowledged / dead-lettered) that
  survives restart and redelivers unacknowledged work on reconnect.
- Core-owned media broker with bounded streaming upload/download, SQLite blob
  persistence, and orphan-upload pruning.
- Bidirectional Tonic/gRPC connector transport; connector session hub with
  registration allowlist and capability gating.
- Owner notices: a connector that loses its session (a WhatsApp logout) reports
  it, and the core relays the alert through the connectors that still work, so a
  bridge that stopped bridging says so instead of going quiet.
- Go connectors: Telegram (`gotd/td`, forum topics, albums), WhatsApp
  (`whatsmeow`, QR pairing), and Instagram (`mautrix-meta` lightspeed).

Reactions, delivery/read receipts, and mirror-thread retitling are executed
end to end. Instagram sends text only; its outbound attachments are not
implemented yet.

## Layout

```text
crates/
  wuphf-model/     Shared domain types (ids, routing, connector, message, dispatch)
  wuphf-proto/     Canonical protobuf contract and Tonic bindings
  wuphf-storage/   Storage trait, in-memory + SQLite backends
  wuphf-core/      Identity, rule matching, fan-out, and event ingestion
  wuphf-runtime/   Tokio event bus, dispatch pipeline, media broker, gRPC
  wuphf-functions/ Remote function broker: provider sessions and Invoke
  wuphf-settings/  Core configuration: loading, env overrides, botconf schema
  wuphf-cli/       Server executable
  wuphf-tui/       Observer TUI (read-only view of the observation log)
connectors/        Go Telegram (gotd/td), WhatsApp (whatsmeow), Instagram connectors
providers/mediaconv/ Sticker converter serving media.convert@1
proto/go/          Generated Go connector bindings
migrations/        SQLite schema migrations
config/            Example server topology and rules
scripts/           Dev session launcher, zellij layout, per-component runners
```

## Run

Every connector authenticates to the core with a token. The core reads each
connector's token from the variable its `[[connectors]].token_env` names and
refuses to start while one is unset, and each connector reads the same value
from the variable its own `core_token_env` names, so every shell needs the same
values. With the example configs, generate each token once (for example with
`openssl rand -hex 16`) and export them from your shell profile or a local env
file you source, never from a tracked file:

```sh
export WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN=<token>
export WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN=<token>
export WUPHF_CONNECTOR_WHATSAPP_RUST_TOKEN=<token>
export WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN=<token>
```

`rules` and `notify` can also change while the core runs, through its
settings service; list each connector's `owner_ids` in the core's config to
choose who may change them from that platform (see `docs/architecture.md`,
"Settings service").

The whole stack at once, in a zellij session with a tab per component:

```sh
./scripts/dev-session.sh
```

Tabs are shells, so a crashed connector leaves you somewhere to restart it
from. Each starts its component by name — `wuphf-server`, `wuphf-telegram`,
`wuphf-whatsapp`, `wuphf-instagram`, `wuphf-tui` — or by the short aliases
`ws`, `wtg`, `wwa`, `wig`, `wtui`. Run `wuphf-help` in any tab for the list.
Re-running the script attaches to the existing session; `--kill` ends it.

Or by hand:

```sh
cargo run -p wuphf-cli -- serve --config config/example.toml

# in another shell, after configuring Telegram credentials
(cd connectors/telegram && go run ./cmd/wuphf-telegram --config config.example.toml)

# in another shell; first launch prints a WhatsApp pairing QR code
(cd connectors/whatsapp && go run ./cmd/wuphf-whatsapp --config config.example.toml)
```

### Rust WhatsApp connector

The text-first Rust connector lives in a separate Cargo workspace because
`whatsapp-rust` and the core require incompatible `libsqlite3-sys` versions:

```sh
cd connectors/whatsapp-rust
cargo build --release
export WUPHF_CONNECTOR_WHATSAPP_RUST_TOKEN=replace-me   # core_token_env
./target/release/wuphf-connector-whatsapp --config config.example.toml
```

First launch pairs through a terminal QR code. Set `phone_number` in the
connector config to use a pair code instead. The connector supports inbound
text, edits, deletes, and reactions plus outbound create/edit/delete; media is
deliberately unsupported in this first version.

## Verify

```sh
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets
(cd proto/go && go test ./...)
(cd connectors/telegram && go test ./...)
(cd connectors/whatsapp && go test ./...)
nix flake check --all-systems
```

See [docs/architecture.md](docs/architecture.md) for the identity and routing
model and [docs/connector-protocol.md](docs/connector-protocol.md) for the
connector session contract.
