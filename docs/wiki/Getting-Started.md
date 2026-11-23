# Getting started

This walks through the usual first bridge: **WhatsApp into a Telegram forum
group**, every WhatsApp chat in its own topic. Instagram works the same way
as WhatsApp. For a server install, see [Operations](Operations.md) instead.

## 1. What you need

- The repository, and [Nix](https://nixos.org) for the dev shell:
  `nix develop` gives cargo, go, protoc, ffmpeg, ImageMagick and the rest.
  Without Nix you need Rust (stable), Go 1.26, `protoc` and a C toolchain.
- A Telegram account, and an app id and hash from
  <https://my.telegram.org> → API development tools.
- A Telegram **forum group** (a group with Topics turned on) to mirror into.
  Its id looks like `-100…`; in wuphf it is written
  `telegram:channel:<number without -100>`.
- The WhatsApp account on your phone (wuphf links as a second device).

## 2. Build

```sh
nix develop
scripts/install-dev-bins.sh      # every binary into scripts/dev-bin/.bin
source scripts/dev-aliases.sh    # ws, wtg, wwa, wig, wmi, wtui, …
```

The `scripts/dev-bin/` wrappers run those binaries from the right directory,
with the repository's `config.local.toml` and `.env.local`.

## 3. Configure the core

```sh
wuphf-server setup
```

This opens a setup screen over `config.local.toml` and `.env.local`. Every
option shows its description. Set:

1. **connectors**: one entry per connector you run, for example
   `whatsapp-main` (platform `whatsapp`) and `telegram-main` (platform
   `telegram`, with `supports_threads` and `supports_topic_create` on). On each
   `token_env`, press `g` to generate the token into `.env.local`.
2. **rules**: one `many-to-one` rule with `source_connector = "whatsapp-main"`
   and `destination` your forum group (`telegram-main:telegram:channel:<id>`).
3. **notify**: a chat where wuphf tells you about trouble, such as your own
   Telegram user (`telegram-main:telegram:user:<your id>`).
4. **owner_ids** on `telegram-main`: your Telegram user id, so you may change
   settings from Telegram.

The [Configuration](Configuration.md) page explains every option.

## 4. Configure and link the connectors

Each connector has its own setup, which also links its account:

```sh
wuphf-telegram setup     # Environment: app id and hash, phone; then Link account
wuphf-whatsapp setup     # Link account: scan the QR code in WhatsApp → Linked devices
```

Keep `connector_id` and `core_token_env` equal to the core's entry for that
connector. Linking also fetches the account's chats into the **chat
directory**, so the core's setup can offer chats by name.

## 5. Run

```sh
./scripts/dev-session.sh
```

This starts a zellij session with a tab for each part, every tab with
`.env.local`'s tokens exported. Or start them by hand, the core first:

```sh
set -a; . ./.env.local; set +a   # the tokens, for every part
ws                               # the core, on config.local.toml
wtg                              # in other shells: Telegram, WhatsApp, …
wwa
```

Send yourself a WhatsApp message. A topic named after the chat appears in the
forum group with an info card, then the message. Answer in the topic, and the
reply goes to WhatsApp.

## 6. Next

- Add Instagram, the sticker converter (`wuphf-mediaconv`, for animated
  stickers) or wuphf-misc: see [Components](Components.md).
- Change settings while it runs: send `/wuphf` in Telegram (see
  [Configuration → Runtime settings](Configuration.md#runtime-settings)).
- Watch everything from a terminal: `wuphf-tui --token <observer_token>`.
- Put it on a server: [Operations](Operations.md).
