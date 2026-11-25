# Configuration

Every part reads one TOML file, and the secrets come from environment
variables. This page explains how that fits together, then every section of
the core's config and what each connector needs. Every option is also
described in its part's example file (`config/example.toml`,
`connectors/*/config.example.toml`, …). Those examples are generated from the
code, so they are always complete and current.

## How configuration works

- **One file per part.** The core reads `--config` (on a server,
  `/etc/wuphf/core.toml`); each connector, provider and wuphf-misc reads its
  own. In the dev tree each part uses `config.local.toml` (ignored by git)
  when there is one, and the checked-in example otherwise.
- **Secrets live in the environment, never in the file.** An option ending in
  `_env` (`token_env`, `core_token_env`, `observer_token_env`, …) holds the
  *name* of an environment variable, and the part reads the value from it.
  In development they are all in one `.env.local` at the repository root; on
  a server, each part has its own `/etc/wuphf/<part>.env`, which systemd
  loads.
- **Environment overrides.** Any core key can also be set by the environment:
  `WUPHF_CORE_` plus the key path in upper case, `__` between levels
  (`WUPHF_CORE_LISTEN`, `WUPHF_CORE_FUNCTIONS__MAX_DEADLINE_MS`). The
  connectors read `WUPHF_LOG_LEVEL`, and Telegram its credentials (below).
- **The setup screens.** `<binary> setup` edits a part's file and the env
  file, with every option's description. `g` generates a token, chats are
  picked by name, and a save is checked the way the part checks it at
  startup. This is the easiest way to write a config.

## Tokens: how the parts trust each other

Every part that connects to the core presents a token, and both sides read it
from a variable **of the same name**. Generate each one once (`g` in setup,
or `openssl rand -hex 32`) and give both sides the same value.

| Who connects | Core side | Its own side |
|--------------|-----------|--------------|
| A connector | `[[connectors]] token_env` | `core_token_env` |
| A function provider (mediaconv, Instagram's `[functions]`) | `[[functions.providers]] token_env` | `token_env` |
| Callers of functions, when the core asks for one | `[functions] invoke_token_env` | `functions_token_env` (connectors), `invoke_token_env` (misc) |
| The TUI, wuphf-misc, `settings reload`/`export`/`import` | `observer_token` | `--token`, `observer_token_env`, `WUPHF_OBSERVER_TOKEN` |

The core refuses to start while a listed connector's variable is unset, and
it accepts a connector only with its own token. A second session for a
connector that is already connected is refused.

## The core's config

### Basics

```toml
listen = "127.0.0.1:50051"            # where every gRPC service listens
database_url = "sqlite://wuphf.sqlite" # messages, media, settings; relative to the working directory
# max_media_bytes = 33554432           # largest file accepted (32 MiB)
# orphan_media_retention_seconds = 86400
# observer_token = "…"                 # required unless listen is local-only
notify = ["telegram-main:telegram:user:704338780"]  # where notices go
# receive_only = ["whatsapp-main:120363000000000000@g.us"]  # nothing sent into these
# mirror_deletes_kept = ["whatsapp-main"]  # deleting a mirror copy keeps the source message
```

Keep `listen` on `127.0.0.1` unless connectors run on other hosts. In that
case put it behind TLS or a VPN, and set `observer_token`.

Some chats take nothing from the bridge (plan 083). The connectors report
them: WhatsApp Status (anything posted there would be a story to every
contact), channels you don't own or admin, and admin-only groups where
you're not an admin. List more in `receive_only` (the source chat or its
mirror topic). A message or an edit typed in such a chat's topic gets 👎 and
a note that deletes itself after 30 seconds; reactions still go (a reaction
to a story reaches its poster as an emoji in their DM). By default, deleting
your message's mirror copy deletes the message on the source too; list a
source connector in `mirror_deletes_kept` to keep it there.

### `[[connectors]]`: who may connect

One entry per connector you run:

```toml
[[connectors]]
id = "telegram-main"                  # = the connector's connector_id
platform = "telegram"                 # whatsapp, telegram, instagram, … or other:<name>
token_env = "WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN"
supports_threads = true               # Telegram only: it can post into topics
supports_topic_create = true          # … and create them
# supports_disappearing_timers = true # WhatsApp: must match the connector's own setting
owner_ids = ["704338780"]             # your account on this platform
```

`owner_ids` is the only way to allow settings changes from a chat, and it can
only be changed in the file.

### `[rules.<id>]`: what mirrors where

```toml
# Every WhatsApp chat into its own topic of one forum group.
[rules.wa-to-telegram]
kind = "many-to-one"
source_connector = "whatsapp-main"
destination = "telegram-main:telegram:channel:2060340733"
# only_chats = ["whatsapp-main:120363012345678901@g.us"]   # an allowlist, or …
# except_chats = ["whatsapp-main:15551234567@s.whatsapp.net"]  # … a blocklist

# One chat into one chat (or one topic): an exception to the rule above.
[rules.family]
kind = "one-to-one"
priority = -1                          # lower wins when rules overlap
source = "whatsapp-main:120363098765432109@g.us"
destination = "telegram-main:telegram:channel:2060340733#7"
```

- A `many-to-one` destination must be on a connector with
  `supports_threads` and `supports_topic_create`.
- When several rules match a chat, the lowest `priority` (default 0) wins,
  then the lowest id.
- The id is the rule's identity: renaming it starts new topics.
- Chats may be written in any address form the platform uses (see
  [Concepts](Concepts.md#one-chat-many-addresses)).

### `[chat_state]`: pin, mute and archive

```toml
[chat_state]
pin = "both"        # off, to_mirror, to_source or both
mute = "both"       # Telegram side needs a user session
archive = "both"    # archive = closed topic on Telegram

[chat_state.chats.work]          # an override for one conversation; any name
chat = "whatsapp-main:120363011111111111@g.us"
mute = "off"
```

Directions are named from the source chat: `to_mirror` means WhatsApp →
Telegram only.

### `[functions]`: providers

```toml
[functions]
# invoke_token_env = "WUPHF_INVOKE_TOKEN"   # set it to require a token from callers

[[functions.providers]]
id = "mediaconv"
token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
functions = ["media.convert"]

[[functions.providers]]           # only for wuphf-misc's Instagram downloader
id = "instagram-main"
token_env = "WUPHF_PROVIDER_INSTAGRAM_TOKEN"
functions = ["instagram.fetch_media"]
```

List only providers you run. A listed provider that is down makes every
animated sticker wait up to the connector's `media_convert_timeout_ms` before
falling back. Deadlines, queue sizes and keepalives have defaults; see the
example file.

### `[updates]`: updates and the release check

```toml
[updates]
# check_interval_seconds = 86400   # how often serve checks; 0 = never
# install_dir = "/usr/local/bin"   # default: beside wuphf-server
# state_dir = "/var/lib/wuphf/update"
# prerelease = false

[updates.wuphf]                    # wuphf's own binaries
# enabled = true
# from = "release"                 # or "source": build the tag on this host

[[updates.sources]]                # a binary you build yourself
name = "my-misc"
repository = "https://codeberg.org/you/wuphf-misc"   # GitHub, Gitea/Forgejo or GitLab
binaries = ["wuphf-misc"]
# forge = "gitea"                  # needed for hosts other than github.com, gitlab.com, codeberg.org
# asset = "{binary}-{os}-{arch}"   # {binary} {tag} {version} {os} {arch} {goarch}; .tar.gz works
# public_key = "RW…"               # its minisign key (else SHA256SUMS only)
# from = "source"
# build = "make OUT=$OUT"          # puts the binaries in $OUT
# services = { "wuphf-misc" = "wuphf-misc.service" }
```

See [Operations → Updates](Operations.md#updates).

## Each connector

Every connector config starts with the same four keys:

```toml
core_address = "127.0.0.1:50051"
connector_id = "whatsapp-main"                          # = the core's [[connectors]] id
core_token_env = "WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN"  # = the core's token_env
log_level = "info"
```

| Connector | What else it needs |
|-----------|--------------------|
| Telegram | `session_file`; `supports_threads`/`supports_topic_create` on. Credentials from the environment: `WUPHF_TELEGRAM_APP_ID`, `WUPHF_TELEGRAM_APP_HASH`, and `WUPHF_TELEGRAM_PHONE` (+ `_PASSWORD` for 2FA) for a user session, or `WUPHF_TELEGRAM_BOT_TOKEN` for a bot. `panel_bot_token` and `[panel]` for a user session's settings panel. |
| WhatsApp (Go or Rust) | `session_database`; `client_mode` (`android` or `android_business`, fixed at pairing); `login_method` (`qr` or `pair_code` with `phone_number`); `supports_disappearing_timers` (matching the core); `[sticker_metadata]` (pack name, author, link). |
| Instagram | `cookies_file` (exported browser cookies: at least `sessionid`, `csrftoken`, `ds_user_id`, `mid`, `ig_did`), `session_file`, `state_file`; `[functions]` to serve wuphf-misc. |

Shared by the connectors:

- `[rendering]`: how copies look on that platform. `attachments_as_files`,
  `max_message_length` (longer text becomes a file), and on Telegram
  `edit_mode` (`in_place` or `repost`) and `delete_mode` (`revoke` or
  `tombstone`).
- `media_convert_timeout_ms` and `functions_token_env` for sticker
  conversion.

The keys marked **Runtime** in a connector's example file can be changed while
it runs (below).

## mediaconv and wuphf-misc

```toml
# providers/mediaconv
core_address = "127.0.0.1:50051"
provider_id = "mediaconv"                    # = the core's [[functions.providers]] id
token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"

# misc
core_address = "127.0.0.1:50051"
observer_token_env = "WUPHF_OBSERVER_TOKEN"  # the core's observer_token
[instagram]
enabled = true
chats = ["whatsapp-main:120363012345678901@g.us"]
# max_per_minute, max_concurrent, dedup_hours, reply_on_failure, caption
```

## Runtime settings

These change while everything runs, without a restart:

- the core's `rules`, `notify`, `receive_only`, `mirror_deletes_kept` and
  `chat_state`;
- each connector's runtime keys (`log_level`, `[rendering]`,
  `[sticker_metadata]`, `media_convert_timeout_ms`), under
  `connector_settings.<connector id>`;
- wuphf-misc's `log_level` and every `[instagram]` option (turn the
  downloader on, pick its chats by name), under `plugin_settings.misc`.

A change is checked like the file and stored in the database as an override.
`unset` returns to the file's value. Ways to change them:

- **In Telegram:** send `/wuphf` for a panel of buttons (the account must be
  in the connector's `owner_ids`). `/wuphf help` lists text commands such as
  `/wuphf set notify ["telegram-main:telegram:user:123"]`.
- **From a terminal:** `wuphf-server settings --config FILE list`, then `get`,
  `set`, `unset`, `add`, `remove`. This edits the database directly, and also
  works while the core is down. Afterwards, `settings reload` (with
  `WUPHF_OBSERVER_TOKEN`) tells a running core to pick the change up.

Everything else (`listen`, `database_url`, the connectors, tokens,
`[functions]`, `[updates]`) is read at startup: change the file and restart.

## Common setups

- **WhatsApp + Instagram into one Telegram forum group:** two `many-to-one`
  rules, one per source connector, with the same `destination`. Each chat
  gets its own topic.
- **Only a few chats:** `only_chats` on the rule.
- **One chat elsewhere:** a `one-to-one` rule with `priority = -1` next to the
  catch-all.
- **Two WhatsApp accounts:** two connectors with different ids, tokens and
  session files, and a rule each.
