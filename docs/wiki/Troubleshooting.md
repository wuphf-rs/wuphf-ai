# Troubleshooting

## Starting up

| Symptom | Cause and fix |
|---------|---------------|
| The core exits: a connector's variable "is unset or empty" | Every `[[connectors]] token_env` must be set. Export `.env.local` (`set -a; . ./.env.local; set +a`), or on a server fill `/etc/wuphf/core.env`. List only connectors you run. |
| A connector keeps retrying with `ALREADY_EXISTS` | Another session of the same connector id is connected (a second copy running). Stop it; the connector gets in once the old session ends. |
| A connector is refused (`unauthenticated`) | Its token differs from the core's: `core_token_env` and the core's `token_env` must hold the same value. |
| `setup`'s Link account or Refresh chats refuses | The connector is running and holds its session's lock (`<session>.lock`). Stop it first. |
| `import` refuses | The core is running and holds its database's lock. Stop it. |

## Messages

| Symptom | Cause and fix |
|---------|---------------|
| Nothing is mirrored | No rule matches the chat: check `rules` (`wuphf-server settings --config FILE get rules`), `only_chats`/`except_chats`, and that `source_connector` is the right id. |
| No topic appears in the forum group | The destination's `[[connectors]]` entry needs `supports_threads` and `supports_topic_create`. A bot must be an admin with "manage topics". |
| A Telegram bot only bridges one way | Privacy mode is on: @BotFather → `/setprivacy` → Disable, or make the bot an admin. |
| A Telegram bot can't reach a user | A bot can't message someone who never started it: have them send `/start` to it. |
| Animated stickers arrive late, or static | mediaconv is listed in `[functions]` but not running. Run `wuphf-mediaconv`, or remove its `[[functions.providers]]` entry so stickers fall back at once. |
| View-once media doesn't arrive from WhatsApp | Set `client_mode` (`android` or `android_business`). It is fixed at pairing: unlink the device on the phone, move the session database aside, link again. |
| Replies from Telegram show "will not disappear" on WhatsApp | Turn on `supports_disappearing_timers` in both the core's entry and the WhatsApp connector's config. |
| Saved contact names or pins are missing (WhatsApp Rust) | App state was not fully fetched. Start it once with `--resync-app-state`. |
| Instagram stopped receiving | The cookies expired. Export fresh ones to `cookies_file`, and move `session_file` aside, with the connector stopped. |

## Settings

| Symptom | Cause and fix |
|---------|---------------|
| `/wuphf` gets no reply | Your Telegram user id isn't in that connector's `owner_ids` (a user session: the session's own account id). |
| A `settings set` doesn't apply | The running core keeps its own copy: run `wuphf-server settings reload` with `WUPHF_OBSERVER_TOKEN`. |
| A file change doesn't apply | `listen`, `database_url`, connectors, tokens, `[functions]` and `[updates]` are read at startup: restart the core. |

## Updates

| Symptom | Cause and fix |
|---------|---------------|
| "no public key to check the signature with" | This build has no release key yet (see [release/README.md](../../release/README.md)), or set `[updates.wuphf] public_key`. |
| "configuration file … does not exist" | Pass the core's config: `--config /etc/wuphf/core.toml`. |
| "another update is running" | One update at a time (`state_dir/update.lock`). |
| A source "could not check" | Network, rate limit (set `token_env` for GitHub), or a wrong `repository`/`forge`. The other sources still update. |
| "the binaries are installed, but some units did not restart" | Check `systemctl status <unit>`; `--rollback` puts the previous binaries back. |

## Still stuck

Raise the log level (`log_level = "debug"` on a connector, `RUST_LOG=debug` on
the core) and watch the TUI. Most "nothing happens" problems are a missing
rule, a token mismatch, or a connector that isn't linked.
