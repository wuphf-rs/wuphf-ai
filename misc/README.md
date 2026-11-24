# wuphf-misc

`wuphf-misc` runs features that are not bridging. It is a client of the core,
like the TUI: it follows the core's live messages (`SubscribeMessages`), calls
functions through the core's function broker, and replies with `SendMessage`.
The core and the connectors know nothing of its features.

A reply is an ordinary owner message: it goes into the chat as you, records
its sent copy, and fans out to that chat's mirrors (a WhatsApp group's
Telegram topic gets the reply too).

## Features

### Instagram downloader

When someone posts an Instagram post or reel link (`/p/…`, `/reel/…`,
`/reels/…`) in one of the configured chats, wuphf-misc replies to that message
with the post's video or photos (every item of a carousel), with
`@author: ` and up to 200 characters of the post's text as the caption.

- Messages you send count too: from your phone or the official apps, or
  typed in the TUI. wuphf-misc's own replies never do, and neither do edits.
- Stories and profiles are not supported.
- Links sent while wuphf-misc is down are not answered.
- The same post in the same chat is answered once in `dedup_hours` (kept in
  memory: a restart forgets it).
- At most `max_per_minute` fetches a minute over every chat (more are
  skipped and logged), and `max_concurrent` at a time.
- A failed fetch is logged; with `reply_on_failure = true` it also replies
  "Could not download this post."
- A medium over 64 MiB or over the core's `max_media_bytes` is left out of the
  reply and logged.

The fetch runs in the Instagram connector, on its logged-in account, as the
function `instagram.fetch_media@1`. To enable it:

1. In the Instagram connector's config, uncomment `[functions]`
   (`provider_id = "instagram-main"`,
   `token_env = "WUPHF_PROVIDER_INSTAGRAM_TOKEN"`).
2. In the core's config, uncomment the matching `[[functions.providers]]`
   entry (`id = "instagram-main"`, `functions = ["instagram.fetch_media"]`).
3. Set `WUPHF_PROVIDER_INSTAGRAM_TOKEN` to one random value for both.
4. Turn it on and list the chats: in the settings panel (below), or in
   wuphf-misc's config with `[instagram] enabled = true` and `chats`.

Chats are chat references, as in the core's rules: `connector:chat`, or
`connector:chat#thread` for one thread only. Use the chat id the TUI shows
for the chat (for a WhatsApp group, its `…@g.us` id); the panel picks them by
name. While none is listed, the downloader answers nowhere.

**Account risk.** Every fetch is an Instagram request on the connector's
account. Instagram can limit or block accounts that fetch much; keep
`max_per_minute` low and the chat list short.

## Settings panel

`log_level` and every `[instagram]` option can change while wuphf-misc runs.
At startup it declares them to the core (`PluginTransport.DeclareSettings`,
with the observer token), which keeps them at `plugin_settings.misc`
(`plugin_id` names another id). Change them:

- **In Telegram:** `/wuphf` → "misc (plugin)": switch the downloader on, pick
  its chats by name, set the limits.
- **From a terminal:** `wuphf-server settings --config FILE set
  plugin_settings.misc.instagram.enabled true`, then `settings reload`.

A change applies to the next message. The config file's values are the base:
unsetting a change returns to them. The other options (`core_address`, the
token variables, `plugin_id`, `reconnect_delay_ms`) need a restart. While the
core cannot be reached, wuphf-misc keeps trying to declare; a core that
refuses (an older core, or one without an observer token) leaves it on its
file's values. wuphf-misc runs with no feature enabled, waiting for one to be
turned on.

## Run

```sh
cp misc/config.example.toml misc/config.local.toml   # enable a feature
export WUPHF_OBSERVER_TOKEN=<the core's observer_token>
(cd misc && go run ./cmd/wuphf-misc -config config.local.toml)
```

In the dev session: `wmi` (it runs on `misc/config.local.toml` when there is
one; `WUPHF_MISC_CONFIG` names another). Under
systemd: `deploy/systemd/wuphf-misc.service`.

**The observer token is full access.** It reads every chat the core has
captured and sends as you into any of them. wuphf-misc posts only into chats
its features configure, but the token itself is not limited; keep it as
secret as the core's own config. Scoped tokens are planned.

## Adding a feature

A feature implements `misc.Feature` (`internal/misc/feature.go`): `Name()`
and `Handle(ctx, message, out)`. The runner calls `Handle` on its own
goroutine for each live message that may trigger features (not an edit, not a
delete, not wuphf-misc's own reply, each message once), and `out.Reply` is the
only way to post. Give the feature its own config table in
`internal/config` with an `enabled` switch, and build it in
`cmd/wuphf-misc/features.go`.
