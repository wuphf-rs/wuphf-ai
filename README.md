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
- Disappearing messages: the core keeps each chat's disappearing-messages
  timer (from messages, timer changes, group notifications and history syncs)
  and both WhatsApp connectors stamp it on what they send, so a reply bridged
  from Telegram disappears like the rest of the chat instead of showing "will
  not disappear". Enable it with `supports_disappearing_timers = true` in the
  core's `[[connectors]]` entry and in the connector's own config, together.
- Chat profiles and notes: the core keeps each chat's title, description,
  picture and member count. A new mirror topic starts with an info card, and
  later changes (description, picture, disappearing timer, members joining,
  leaving, promoted or demoted) appear as notes in the topic; a rename renames
  the topic. Both WhatsApp connectors report them, show saved contact names
  for senders and @mentions, and name a DM the owner starts after the contact.
- Pin, mute and archive sync both ways between a WhatsApp chat and its
  Telegram mirror topic (pinned topic, topic notifications, closed topic),
  each configurable as `off`, `to_mirror`, `to_source` or `both` under
  `[chat_state]`, with per-conversation overrides.
- Go connectors: Telegram (`gotd/td`, forum topics, albums), WhatsApp
  (`whatsmeow`, QR pairing), and Instagram (`mautrix-meta` instameow).
- `wuphf-misc`: features that are not bridging, outside the core. The first
  posts the video or photos of an Instagram post or reel linked in a chat you
  choose (see `misc/README.md`).

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
misc/              wuphf-misc: non-bridging features (Instagram downloader)
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
export WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN=<token>
```

`rules`, `notify` and `chat_state` (`chat_state.pin`, `.mute`, `.archive`
and the `chat_state.chats` overrides) can also change while the core runs,
through its settings service; list each connector's `owner_ids` in the core's config to
choose who may change them from that platform (see `docs/architecture.md`,
"Settings service").

From a terminal, `wuphf settings` views and changes them in the core's
database directly, so it also works while the core is down. It loads the
config the same way `serve` does, so it needs the same token variables:

```sh
cargo run -p wuphf-cli -- settings --config config/example.toml list
cargo run -p wuphf-cli -- settings get rules
cargo run -p wuphf-cli -- settings set notify '["telegram-main:telegram:user:704338780"]'
cargo run -p wuphf-cli -- settings unset notify          # back to the file's value
# tell a running core to re-read the config file and the stored settings
WUPHF_OBSERVER_TOKEN=<observer_token> cargo run -p wuphf-cli -- settings reload
```

The whole stack at once, in a zellij session with a tab per component:

```sh
./scripts/dev-session.sh
```

Tabs are shells, so a crashed connector leaves you somewhere to restart it
from. Each starts its component by name — `wuphf-server`, `wuphf-telegram`,
`wuphf-whatsapp`, `wuphf-instagram`, `wuphf-misc`, `wuphf-tui` — or by the
short aliases `ws`, `wtg`, `wwa`, `wig`, `wmi`, `wtui`. Run `wuphf-help` in any tab for the list.
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

The Rust connector lives in a separate Cargo workspace because
`whatsapp-rust` and the core require incompatible `libsqlite3-sys` versions:

```sh
# First uncomment the whatsapp-rust [[connectors]] entry in the core config.
cd connectors/whatsapp-rust
cargo build --release
export WUPHF_CONNECTOR_WHATSAPP_RUST_TOKEN=replace-me   # core_token_env
./target/release/wuphf-connector-whatsapp --config config.example.toml
```

First launch pairs through a terminal QR code. Set `phone_number` in the
connector config to use a pair code instead. Inbound, the connector bridges
text, photos, videos, GIFs, voice notes, documents and stickers (captions
become the text; view-once media is marked `👁️ View once`), edits, deletes and
reactions, including those inside disappearing-messages wrappers. Outbound it
sends text, edits and deletes; outbound media is still to come. It registers
as an Android client so WhatsApp delivers view-once media: set `client_mode`
to `"android"` for a personal account or `"android_business"` for a WhatsApp
Business account. The mode is sent at pairing, so changing it needs a fresh
pairing (unlink the device on the phone, move the session database aside,
start again).

It names mirror threads like the Go connector (`[G]` group subject, `[P]` saved
name, else push name and number, from its own `wuphf_contacts` table in the
session database), reports chat profiles (group subject, description, members,
pictures) for the core's info cards and notes, syncs pin, mute and archive with
the mirror topics, and stamps disappearing timers when
`supports_disappearing_timers = true` is set in both
`connectors/whatsapp-rust/config.example.toml` (or your copy) and the core's
`whatsapp-rust` `[[connectors]]` entry.

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
