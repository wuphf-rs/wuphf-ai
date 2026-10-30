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
  be addressed without preconfiguration. A user session also primes it from
  its dialog list; a bot session looks up a peer it has not seen
  (`channels.getChannels` / `users.getUsers`) on its first send, since
  Telegram rejects a zero hash for media
- the session kind (bot or user) comes from the account the stored session is
  logged in as, not from which credentials are set. When the two disagree the
  connector says so at startup and runs as the stored account; delete the
  session file to log in again
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
# The core token: the same value the core reads from this connector's
# [[connectors]].token_env, named here by core_token_env.
export WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN=replace-me
```

Run the core server, then the connector from this directory:

```sh
go run ./cmd/wuphf-telegram --config config.example.toml
```

The keys marked "Runtime." in `config.example.toml` can also change while the
connector runs, from the core's settings panel or `wuphf settings`: the
connector declares them to the core and applies each change the core pushes
(see `docs/connector-protocol.md`, "A connector's own settings").

## Settings panel

`/wuphf` (`panel.command` changes it) opens the core's settings as a panel of
buttons, like `wuphf settings` in a chat. The command is taken before
anything is mirrored and is never mirrored, whoever sends it. The connector
only says who is acting; the core obeys the Telegram user ids in its
`[[connectors]].owner_ids` for this connector and ignores everyone else (an
unauthorized `/wuphf` gets no reply, an unauthorized button press gets a
refusal toast).

- `/wuphf` opens the panel; `/wuphf help` lists the text subcommands, e.g.
  `/wuphf set notify ["telegram-main:telegram:user:123"]`.
- When the panel asks for a value, answer with your next message in that chat
  (`/cancel` gives up). The answer is not mirrored, and it and the question
  are deleted once answered.
- The command message is deleted once the panel opens, where the account may
  delete it.

**Bot sessions** draw the panel with an inline keyboard directly.

**User sessions** cannot attach buttons to their messages, so the panel goes
through a **helper bot** in inline mode: the account asks the helper bot for
an inline result holding the panel and sends it, and Telegram shows "via
@<helper bot>" under it. Presses reach the helper bot, which the connector
runs as a second client in the same process. To set it up:

1. Create a bot with @BotFather and turn on inline mode (`/setinline`).
2. Give the connector its token in `panel_bot_token` or
   `WUPHF_TELEGRAM_PANEL_BOT_TOKEN`, and a `[panel] session_file` for its
   session.
3. Add the user session's own account id to the core's `owner_ids` for this
   connector: in a user session, only the account itself drives its panel.

Without a helper bot, or when the trip through it fails (inline bots
forbidden in a group, a timeout), the panel is sent as text with numbered
options; replying with a number presses one.

**Messaging the helper bot.** When the bridge runs on an account that is not
yours, open the panel by sending `/wuphf` to the helper bot in a private chat
from your own account. There it is an ordinary bot panel with an inline
keyboard, and the core obeys you if your Telegram user id is in `owner_ids`;
anyone else gets no reply. The helper lists the command in its menu for
private chats. It ignores groups it is in, and nothing it receives is
mirrored: it is not the bridge account.

## Sticker labels

Telegram stickers take no caption, so a mirrored sticker names its sender
another way. A bot session puts the name ("🗣️: Alice") on an inline button
under the sticker.

A user session uses the settings panel's helper bot for the same button: the
helper uploads the sticker and offers it as an inline result carrying the
button, and the account sends that result. It is one message, so deleting the
mirrored sticker leaves nothing behind, and Telegram shows "via @<helper bot>"
under it. Without a helper bot, or when the helper fails or takes longer than
5 seconds (delivery is sequential, so a stuck helper must not hold up the
chat), the account posts the name as a short message and replies the sticker
to it. Stickers the owner sends directly carry no label and are sent as usual.

## Animated stickers

Telegram shows an animated WebP sticker only as a still frame, so the connector
converts animated stickers to WebM video stickers before sending them. It does
not convert them itself: it calls the core's `media.convert@1` function, which
the `providers/mediaconv` process serves (that process needs `ffmpeg` and
ImageMagick; this connector needs neither). When the conversion fails or no
converter is available, the sticker is sent as a static sticker.

Delivery on a connector is sequential, so a conversion holds back the messages
queued behind it. If the core's `[functions]` configuration allows a
`media.convert` provider that is not running, every animated sticker waits up
to `media_convert_timeout_ms` (default 20 s) before falling back. If you do not
run the provider, remove its `[[functions.providers]]` entry from the core
configuration: calls then fail at once and the sticker falls back immediately.

When the core sets `invoke_token_env`, set `functions_token_env` here to the
name of an environment variable holding the same token.
