# Components

wuphf is a set of separate programs. Run the core, then only the parts you
need. Every binary prints its version with `--version`, and every one with a
config has a `setup` screen.

| Binary | Role | Config | Needs |
|--------|------|--------|-------|
| `wuphf-server` | The core: routing, identity, storage, every gRPC service | `config/example.toml` | — |
| `wuphf-tui` | A terminal view of every captured chat; send as the owner | flags only | the observer token |
| `wuphf-telegram` | Telegram connector (user or bot session, forum topics) | `connectors/telegram/config.example.toml` | an app id and hash |
| `wuphf-whatsapp` | WhatsApp connector (Go, whatsmeow) | `connectors/whatsapp/config.example.toml` | — |
| `wuphf-whatsapp-rust` | WhatsApp connector (Rust, whatsapp-rust), an alternative to the Go one | `connectors/whatsapp-rust/config.example.toml` | — |
| `wuphf-instagram` | Instagram connector (mautrix-meta instameow) | `connectors/instagram/config.example.toml` | session cookies |
| `wuphf-mediaconv` | Provider of `media.convert@1`: animated sticker conversion | `providers/mediaconv/config.example.toml` | `ffmpeg`, ImageMagick |
| `wuphf-misc` | Features that are not bridging (the Instagram downloader) | `misc/config.example.toml` | the observer token |

## The core: `wuphf-server`

It holds everything that matters: rules, identity, the message store
(SQLite), media, the settings, and the services the other parts connect to.
Its subcommands:

| Command | Does |
|---------|------|
| `serve --config FILE` | Run the core. |
| `setup` | Edit the config and env file on a setup screen. |
| `settings list/get/set/unset/add/remove` | Change runtime settings in the database (works while the core is down). |
| `settings reload` | Tell a running core to re-read its file and settings. |
| `settings export/import` | Back up and restore settings through a running core. |
| `export`, `import` | Back up and restore the config and every stored setting, offline. |
| `update` | Update every installed binary to its newest release ([Operations](Operations.md#updates)). |
| `functions list` | Show the providers and functions a running core has. |

## Connectors

What each one carries across. "In" is from the platform into wuphf; "out" is
what it does when wuphf sends to it.

| | Telegram | WhatsApp (Go) | WhatsApp (Rust) | Instagram |
|--|:--:|:--:|:--:|:--:|
| Text in / out | ✓ / ✓ | ✓ / ✓ | ✓ / ✓ | ✓ / ✓ |
| Media in / out (photos, video, files, voice) | ✓ / ✓ | ✓ / ✓ | ✓ / ✓ | ✓ / text only |
| Stickers (animated ones via mediaconv) | ✓ | ✓ | ✓ | in |
| Replies | ✓ | ✓ | ✓ | ✓ |
| Edits and deletes out | ✓ | ✓ | ✓ | ✓ |
| Reactions in | ✓ | ✓ | ✓ | ✓ |
| Reactions out | as text | yours | yours | yours |
| Delivered / read receipts out | 👍 / 👀 (DMs) | — | — | — |
| Topics (a mirror destination) | ✓ | — | — | — |
| Pin, mute, archive sync | ✓ (mute: user session) | ✓ | ✓ | — |
| Disappearing timers | — | ✓ | ✓ | — |
| Chat profiles (title, picture, members) | topic names | ✓ | ✓ | titles |
| Link the account from `setup` | ✓ | ✓ (QR or pair code) | ✓ (QR or pair code) | ✓ (cookies) |
| Settings panel in chat | `/wuphf` | — | — | — |

"Yours" means only your own reactions are applied, as your account; anyone
else's is shown on Telegram only. In Telegram, replying to a message with a lone
emoji reacts with it, so any emoji works. A connector that can't apply something (a
read receipt on WhatsApp) acknowledges it, so nothing piles up.

### Telegram

The mirror side. It runs as a **user session** (your account, logs in with
phone, code and 2FA) or a **bot session** (a @BotFather token). A user
session can mute topics and is the usual choice. The settings panel is
`/wuphf`; a user session shows it through a small helper bot
(`panel_bot_token`). More: [Telegram README](../../connectors/telegram/README.md).

A bot needs privacy mode off, admin rights with "manage topics" in the forum
group, and can't message someone who never started it.

### WhatsApp: Go or Rust

Two independent connectors for the same job; run **one** per WhatsApp
account. Both link as a linked device (QR code or pair code), link again by
themselves after a logout (sending you the code as a notice), keep
disappearing timers, report chat profiles, and sync pin, mute and archive.
`client_mode` registers as an Android client so view-once media arrives.
More: [WhatsApp README](../../connectors/whatsapp/README.md), and the
[README's Rust WhatsApp section](../../README.md#rust-whatsapp-connector).

### Instagram

Logs in with your browser's session cookies (`cookies_file`) and resumes
after a restart from where it stopped. It sends text only; media it receives
is bridged. It can also serve `instagram.fetch_media@1` for wuphf-misc. More:
[Instagram README](../../connectors/instagram/README.md).

## Providers: `wuphf-mediaconv`

Converts animated stickers between platforms: WhatsApp animated WebP to
Telegram video stickers, and Telegram video and Lottie stickers to WhatsApp.
Without it, those stickers arrive static or as files. More:
[mediaconv README](../../providers/mediaconv/README.md).

## Clients: `wuphf-tui` and `wuphf-misc`

Both use the observer token, which reads every chat and sends as you. Keep it
secret.

- `wuphf-tui --server http://127.0.0.1:50051 --token <observer_token>`:
  browse every captured chat and send as the owner.
- `wuphf-misc`: when someone posts an Instagram post or reel link in a chat
  you choose, it replies with the video or photos. Its settings are in
  `/wuphf` under "misc (plugin)". More:
  [wuphf-misc README](../../misc/README.md).
