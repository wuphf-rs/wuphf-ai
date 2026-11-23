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

### First-time setup

`setup` opens a setup screen over a component's config file and the env file
its options name, without anything running: every option with its
description, default and kind, tables and lists opened as screens, and values
checked as they are typed. An option that names an environment variable (a
token) sets that variable's value in the env file, and `g` generates a token.
A save is checked by the component's own loader first (the core refuses a
config whose connector tokens are unset, as it would at startup), then written
whole, laid out like the example: comments of your own in the config file are
not kept, and keys the component does not know are kept commented out at the
end. The env file keeps its other lines, and a new one is readable by its
owner only.

```sh
wuphf-server setup                  # config.local.toml and .env.local at the repo root
wuphf-telegram setup                # each component: its config.local.toml, the repo's .env.local
wuphf-whatsapp setup
wuphf-whatsapp-rust setup
wuphf-instagram setup
wuphf-mediaconv setup
wuphf-misc setup
```

Telegram's application credentials (`WUPHF_TELEGRAM_APP_ID`,
`WUPHF_TELEGRAM_APP_HASH`, `WUPHF_TELEGRAM_PHONE`, `_PASSWORD`, the bot
tokens) are read from the environment without an option naming them: its
setup sets them on its Environment screen (the root screen's Environment
row), in the env file, and a save is refused while the app id or hash is
missing.

A first setup also links each connector's account, on its setup screen
(**Link account**, at the end of the root screen), so the connector starts
linked: WhatsApp (both connectors) by a QR code drawn there or a pair code,
by `login_method`; a Telegram user session by its phone, the login code and
the 2FA password (`WUPHF_TELEGRAM_PHONE` and `_PASSWORD` are used when set);
a Telegram bot by checking its token; Instagram by the session cookies, pasted
as JSON or named by a file and written to `cookies_file`. The account's chats
are then fetched into the chat directory (below); **Refresh chats** fetches
them again. Both act on the saved config, and refuse while the connector runs:
a running connector holds its session's lock (`<session>.lock`). A WhatsApp QR
code needs a terminal of about 70 × 40.

Chats are picked by name. The core's chat options (`notify`, the rules'
chats, chat-state overrides) and misc's `[instagram] chats` suggest chats as
you type, from the **chat directory**: one file per connector,
`<connector_id>.chats.json`, in `$WUPHF_CHAT_DIRECTORY`, else
`$XDG_STATE_HOME/wuphf/chats` (`~/.local/state/wuphf/chats`). A connector's
setup writes its file when its account is linked there, and Refresh
chats rewrites it; the core's setup offers the chats of the connectors its
config lists, misc's those of every file. A typed
`connector:chat` is still taken, so a chat the directory lacks can be named
by hand.

The core and the connectors read a connector's token from variables of the
same name, so a token generated on one side is set for the other too. Each
component's `config.example.toml` (the core's `config/example.toml`) is
generated from its config type, which describes every option once; a test
fails when they differ (`WUPHF_WRITE_EXAMPLES=1 cargo test -p wuphf-settings
example` rewrites the core's).

### Backups

The core's settings live in two places: its config file and the overrides in
its database (changed from a settings panel or `wuphf-server settings`),
every connector's included (`connector_settings.<id>`). One backup holds
both:

```sh
wuphf-server export -o wuphf-backup.toml                 # no token values
wuphf-server export -o wuphf-backup.toml --with-secrets  # with .env.local's tokens
wuphf-server import wuphf-backup.toml                    # the core must be down
```

A backup reads as the config file, every override applied, then a
`[backup]` table with the connectors' settings (and with `--with-secrets`
the values of the token variables the options name, in `[backup.env_file]`;
the file is written readable by its owner only). `import` checks the
restored file as the core would load it, keeps the previous config file
and env file as `.bak`, writes them, and replaces the stored overrides; a
backup without secrets leaves the env file and the file's secret options as
they are. It refuses while a core runs: `serve` holds its database's lock
(`<database>.lock`). Both commands take `--config` (default
`config.local.toml`) and `--env-file` (default `.env.local`).

Through a running core, never with secrets (`WUPHF_OBSERVER_TOKEN` is the
token):

```sh
wuphf-server settings export -o wuphf-backup.toml
wuphf-server settings import wuphf-backup.toml --dry-run  # what it would set
wuphf-server settings import wuphf-backup.toml
```

The online import sets every runtime setting and connector setting whose
value differs, and lists what differs but only the offline import restores
(the listen address, the connectors, …). `wuphf-server setup` has the same
as Export settings and Import settings at the end of its screen.

### Versions and updates

Every binary prints its version (`wuphf-server --version`,
`wuphf-telegram version`): one version for the whole project, released as
a tag `vX.Y.Z` with binaries for linux `x86_64` and `aarch64`
([release/README.md](release/README.md)). One command updates every
installed binary:

```sh
sudo wuphf-server update --config /etc/wuphf/core.toml --check  # what would change
sudo wuphf-server update --config /etc/wuphf/core.toml          # asks, then installs
sudo wuphf-server update --config /etc/wuphf/core.toml --rollback
```

It looks for the binaries beside `wuphf-server` (or `[updates] install_dir`),
asks each one its version, and fetches the newest release. Every download
must be in that release's `SHA256SUMS`, and the file must carry a valid
minisign signature from wuphf's release key for that exact tag. Nothing is
installed until every download has passed. Each binary is then replaced
atomically, the old one kept for `--rollback`, and the systemd units that
were running an updated binary are restarted, the core first. `--only
BINARY` limits it to one binary (and installs that binary if it isn't there
yet), `--from-source` builds the release's tag instead of downloading it
(this needs git, cargo, go and a C toolchain), and `--yes` skips the
question.

Parts built elsewhere, such as your own `wuphf-misc` or a connector of your
own, are update sources of their own. They can live on GitHub, Gitea/Forgejo
(Codeberg) or GitLab. A binary a source lists is never touched by wuphf's own
releases:

```toml
[[updates.sources]]
name = "my-misc"
repository = "https://codeberg.org/you/wuphf-misc"
binaries = ["wuphf-misc"]
asset = "{binary}-{version}-{os}-{goarch}.tar.gz"  # default "{binary}-{os}-{arch}"
public_key = "RW..."   # its minisign key; without one, only SHA256SUMS is checked
# from = "source"; build = "make OUT=$OUT"   # build its tag instead
```

While it runs, the core checks for new releases once a day
(`[updates] check_interval_seconds`, `0` turns it off). It tells the owner
about each new one once, through the notice chats (`notify`), with the
command that installs it. The check only reads; it never installs anything.

### Tokens and settings

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
# First add a whatsapp-rust [[connectors]] entry to the core config
# (`wuphf-server setup`: Connectors, a; token_env
# WUPHF_CONNECTOR_WHATSAPP_RUST_TOKEN, platform whatsapp).
cd connectors/whatsapp-rust
cargo build --release
export WUPHF_CONNECTOR_WHATSAPP_RUST_TOKEN=replace-me   # core_token_env
./target/release/wuphf-connector-whatsapp --config config.example.toml
```

It links by `login_method`: a QR code (the default) or, with `phone_number` set,
a pair code typed into WhatsApp. The code is printed in the terminal and sent
to the owner through the core's notice chats (the QR code as a picture), at
the first launch and again by itself whenever WhatsApp logs the device out:
the dead session database is moved aside as
`whatsapp-rust.session.logged-out-<seconds>.db` and a new one links at the same path,
while messages for WhatsApp wait in the core. Inbound, the connector bridges
text, photos, videos, GIFs, voice notes, documents and stickers (captions
become the text; view-once media is marked `👁️ View once`), edits, deletes and
reactions, including those inside disappearing-messages wrappers. It reports
delivered and read receipts in direct chats, and tells the owner through the
core when the WhatsApp session is lost (a logout, another client taking over,
a temporary ban, an outdated client), when it is back, and when an incoming
message's media could not be downloaded. Outbound it
sends text, edits, deletes and media of every kind (photos, videos, GIFs,
audio, files, stickers), several attachments as consecutive messages with the
text on the first, and follows `[rendering]` as the Go connector does
(`attachments_as_files`, `max_message_length`). Stickers go out as WhatsApp
stickers carrying the `[sticker_metadata]` pack name, author and link (the
EXIF is written in Rust, so no `webpmux` is needed); animated stickers in
another format (Telegram video and Lottie stickers) are first converted to
animated WebP by the core's `media.convert` provider (`providers/mediaconv`),
and arrive as files when it is not running or takes longer than
`media_convert_timeout_ms` (default 20000). Set `functions_token_env` when
the core sets `[functions] invoke_token_env`. Its runtime settings are the Go
connector's (`log_level`, `media_convert_timeout_ms`, `[sticker_metadata]`,
`[rendering]`, with the same keys and choices): it declares them when it
registers, so they can be changed from the settings panel or `wuphf settings`
under `connector_settings.<connector id>` and apply without a restart; a
changed log level applies at once unless `WUPHF_LOG_LEVEL` is set. Its section
in the settings panel (`/wuphf`) also has two buttons while it is connected, as
the Go connector's does: "🔄 Resync app state" fetches every app-state
collection (contacts, pins, mutes, archives) whole again, and "🔗 Send a new
link code" sends the next link code at once rather than at the next gap while
it waits to be linked. It registers
as an Android client so WhatsApp delivers view-once media: set `client_mode`
to `"android"` for a personal account or `"android_business"` for a WhatsApp
Business account. The mode is sent at pairing, so changing it needs a fresh
pairing (unlink the device on the phone, move the session database aside,
start again).

It names mirror threads like the Go connector (`[G]` group subject, `[P]` saved
name, else push name and number, from its own `wuphf_contacts` table in the
session database, `[C]` channel name), reports chat profiles (group subject, description, members,
pictures) for the core's info cards and notes, syncs pin, mute and archive with
the mirror topics, and stamps disappearing timers when
`supports_disappearing_timers = true` is set in both
`connectors/whatsapp-rust/config.example.toml` (or your copy) and the core's
`whatsapp-rust` `[[connectors]]` entry.

Its ids are canonical exactly as the Go connector's: the device is stripped and
a phone number becomes its LID when the mapping is known, and the core is told
when a chat's address changes, so a connector id can move between the two
implementations without splitting conversations. Outbound, each chat is
addressed in the form WhatsApp uses for it, and a reply quotes the real author.

Saved contact names and pin / mute / archive come from WhatsApp's app-state
sync, which the library fetches in full only once per link. A session the
connector did not link itself (or linked before it read a collection) is
fetched in full again once, on its first connect; to force that for every
collection, start it with `--resync-app-state` (`wwr --config
config.local.toml --resync-app-state`).

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
