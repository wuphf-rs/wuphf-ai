# Operations

Running wuphf on a server: installing, updating, backing up, and keeping an
eye on it.

## Installing on a server

The [systemd guide](../../deploy/systemd/README.md) is the full procedure. In
short:

1. A `wuphf` system user.
2. Binaries in `/usr/local/bin`: download `wuphf-server` from a release and
   check it once by hand, then let it install the rest
   (`wuphf-server update --only …`). Or build them all with
   `scripts/build-release.sh`.
3. Configs in `/etc/wuphf/<part>.toml`, secrets in `/etc/wuphf/<part>.env`
   (mode `0600`, owner root), state in `/var/lib/wuphf/<part>`.
4. The units from `deploy/systemd/`, grouped under `wuphf.target`.
5. The first logins (WhatsApp pairing, a Telegram user session), done once in
   a terminal, as the systemd guide shows.

```sh
sudo systemctl start wuphf.target
journalctl -fu 'wuphf-*'
```

Each unit runs sandboxed and read-only outside its state directory. The
connectors stop on SIGINT so they save their sessions.

## On NixOS

The flake has every binary as a package and a NixOS module. It runs one
core, any number of connectors and providers (by id), and wuphf-misc, each as
a service with the same user, state directories and hardening as the systemd
units:

```nix
{
  inputs.wuphf.url = "github:wuphf-rs/wuphf-ai";   # or a release tag: /v0.2.0

  outputs = { nixpkgs, wuphf, ... }: {
    nixosConfigurations.server = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        wuphf.nixosModules.default
        {
          services.wuphf = {
            secretsDirectory = "/run/secrets";   # wuphf-<id>.env for each
            core = {
              enable = true;
              settings = {            # config/example.toml, as Nix
                notify = [ "telegram-main:telegram:user:704338780" ];
                rules.wa-to-telegram = {
                  kind = "many-to-one";
                  source_connector = "whatsapp-jio";
                  destination = "telegram-main:telegram:channel:2060340733";
                };
              };
            };
            connectors = {
              telegram-main = {
                type = "telegram";
                threads = true;
                topicCreate = true;
                ownerIds = [ "704338780" ];
                settings.session_file = "./telegram.session.json";
              };
              # Two accounts of one kind: one connector each.
              whatsapp-jio = { type = "whatsapp-rust"; disappearingTimers = true; };
              whatsapp-airtel = { type = "whatsapp-rust"; disappearingTimers = true; };
              instagram-main = {
                type = "instagram";
                functions = [ "instagram.fetch_media" ];   # for wuphf-misc
              };
            };
            providers.mediaconv.type = "mediaconv";
            misc.enable = true;
          };
        }
      ];
    };
  };
}
```

- **A connector is described once.** `type` is one of `telegram`,
  `whatsapp`, `whatsapp-rust` or `instagram`. The module writes:
  - the core's `[[connectors]]` entry;
  - the connector's `connector_id` (its id), `core_address` and
    `core_token_env`;
  - the flags both sides must agree on: `threads`, `topicCreate` and
    `disappearingTimers`, each accepted only by the kinds that have it.

  `settings` holds the rest of its config file
  (`connectors/<kind>/config.example.toml`). `coreSettings` adds keys to its
  core entry. With `functions`, it also serves functions to the core under
  its id (Instagram only).
- **Providers** (`providers.<id>`, type `mediaconv`) are wired the same way
  into `[[functions.providers]]`. **wuphf-misc** presents the core's observer
  token.
- **Tokens:** each connector's is `WUPHF_CONNECTOR_<ID>_TOKEN`, and each
  provider's `WUPHF_PROVIDER_<ID>_TOKEN` (`whatsapp-jio` →
  `WHATSAPP_JIO`). Each lives only in that unit's `environmentFile`: by
  default `<secretsDirectory>/wuphf-<id>.env`, or set it per unit. Keep
  these files out of the store; sops-nix or agenix work. The core loads
  every connector's and provider's file, and its own `wuphf-core.env` holds
  `WUPHF_CORE_OBSERVER_TOKEN=…`, which wuphf-misc reads too.
- Each unit is `wuphf-<id>` (`wuphf-core`, `wuphf-whatsapp-jio`, …), with its
  state in `/var/lib/wuphf/<id>`. Relative paths in its settings resolve
  there.
- First logins: stop the unit, then run its binary's `setup` (Link account),
  or run it once by hand as `wuphf` in its state directory, as the
  [systemd guide](../../deploy/systemd/README.md#first-login) shows.
- **Updates come from the flake** (`nix flake update wuphf`, then rebuild).
  `wuphf-server update` cannot write to the store, so the module turns the
  core's release check off by default.
- **The binary cache:** CI builds every package natively for x86_64-linux and
  aarch64-linux on every push to `main` (a release tag is a `main` commit),
  and pushes them to Cachix. Add the cache as a substituter (its name and
  public key are on its Cachix page) to download instead of build:

  ```nix
  nix.settings = {
    substituters = [ "https://<cache>.cachix.org" ];
    trusted-public-keys = [ "<cache>.cachix.org-1:…" ];
  };
  ```

  The same holds when you deploy from an x86_64 laptop to an aarch64
  machine (`nixos-rebuild --target-host`, cross-compiled or not): the
  module asks for the native aarch64 builds, which the laptop downloads and
  copies over, so nothing of wuphf is built. For a commit CI has not built,
  or a system on another nixpkgs, build them with that system's own nixpkgs
  instead (cross-compiled on the laptop for a cross-built system):

  ```nix
  nixpkgs.overlays = [ wuphf.overlays.default ];
  services.wuphf.packages = pkgs.wuphf-packages;
  ```

## With Docker

`ghcr.io/wuphf-rs/wuphf` holds every binary of a release (tags `0.2.0`,
`0.2`, `latest`; amd64 and arm64). `deploy/docker/compose.yaml` runs the
core plus the parts named in `COMPOSE_PROFILES`. The
[Docker guide](../../deploy/docker/README.md) is the full procedure. In
short:

```sh
cd deploy/docker && cp .env.example .env    # version, parts, PUID/PGID
docker compose run --rm -it --no-deps core \
    wuphf-server setup --config /etc/wuphf/core.toml --env-file /etc/wuphf/core.env
docker compose run --rm -it whatsapp \
    wuphf-whatsapp setup --config /etc/wuphf/whatsapp.toml --env-file /etc/wuphf/whatsapp.env
docker compose up -d
```

- Two host directories hold everything. `config/` holds `<part>.toml` and
  `<part>.env`. `data/<part>/` is that part's state and working directory,
  and `data/chats/` is the shared chat directory. Both are owned by
  `PUID:PGID`.
- The other parts reach the core at `core:50051`. Set `observer_token`,
  because the core listens on the compose network.
- **Updates:** a new `WUPHF_VERSION`, then `docker compose pull && docker
  compose up -d`. The image's release check is off.

## Updates

Every binary reports one project version (`wuphf-server --version`). A
release is a tag `vX.Y.Z` with binaries for linux x86_64 and aarch64, and a
`SHA256SUMS` signed with wuphf's release key.

```sh
sudo wuphf-server update --config /etc/wuphf/core.toml --check   # what would change
sudo wuphf-server update --config /etc/wuphf/core.toml           # asks, then installs
sudo wuphf-server update --config /etc/wuphf/core.toml --rollback
```

- **Always pass the core's config.** Its `[updates]` section lists your own
  sources. Without them, `update` would treat your custom `wuphf-misc` as
  wuphf's own.
- **Nothing installs until every download is checked:** each one's SHA-256
  must be in `SHA256SUMS`, and that file's signature must name the release.
  Binaries are replaced atomically, the old ones kept for `--rollback`, and
  the units that were running are restarted, the core first.
- **Options:** `--only BINARY` limits the update to one binary (and installs
  it if missing), `--from-source` builds the tag here, `--no-restart` leaves
  the units alone, and `--yes` skips the question. Add
  `--env-file /etc/wuphf/core.env` when a source names a token variable.
- **A GitHub token** is optional: `[updates.wuphf] token_env` names one, for
  a private fork or past GitHub's 60 unauthenticated API requests an hour.
- **Your own binaries:** a binary you build yourself is an
  `[[updates.sources]]` entry, published on GitHub, Gitea/Forgejo or GitLab
  ([Configuration](Configuration.md#updates-updates-and-the-release-check)).
- **The release check:** the core checks once a day and sends one notice per
  new release to `notify`, with the exact command to run. It never installs
  anything by itself.

Making a release: [release/README.md](../../release/README.md).

## Backups

The settings live in two places: the config file, and the overrides stored in
the database. One backup holds both:

```sh
wuphf-server export --config /etc/wuphf/core.toml -o backup.toml                 # no secrets
wuphf-server export --config /etc/wuphf/core.toml -o backup.toml --with-secrets --env-file /etc/wuphf/core.env
wuphf-server import backup.toml --config /etc/wuphf/core.toml                     # core stopped
```

`import` checks the backup first, and keeps the previous files as `.bak`. It
refuses while the core runs, because the core holds its database's lock.
Through a running core, without secrets: `wuphf-server settings export` and
`import` (`--dry-run` shows what would change).

The database itself (`database_url`) holds the message history and its
mapping between platforms. Back it up with the core stopped, or with
`sqlite3 wuphf.sqlite ".backup copy.sqlite"`. Each connector's session file or
database is its login: back those up too to avoid linking again.

## Watching it

- **Notices** come to the `notify` chats: a connector lost its session or is
  back, WhatsApp needs linking (with the QR code), media could not be
  downloaded, a release is out. List targets on two platforms.
- **Logs:** `journalctl -u wuphf-core` and so on. Raise a connector's
  `log_level` at runtime (`/wuphf` or `wuphf-server settings`) without a
  restart. The core uses `RUST_LOG` (`RUST_LOG=wuphf_runtime=debug,info`).
- **The TUI** shows every captured chat live:
  `wuphf-tui --server http://127.0.0.1:50051 --token <observer_token>`.
- **Providers:** `wuphf-server functions list` shows which are connected.
- **Health checks** (below) report what stops messages getting through.

## Health checks

The core checks that each rule can deliver. It asks the connectors about
the chats the rules use (a *probe*), and it does this when a connector
connects, when the rules change, and when you ask
(`wuphf-server health --run`). What it checks:

- **Rule targets:** the chat exists, the account is in it and may post.
  Where the rule creates topics (a many-to-one into Telegram), the group
  has Topics turned on and the account may create topics (the Manage Topics
  admin right, or "anyone may create topics").
- **Rule sources a rule names** (a one-to-one's chat, a many-to-one's
  `only` list): the account is still in the chat and may post there. An
  admin-only WhatsApp group blocks replies typed in its topic.
- **Wiring:** a connector away for 15 minutes; a function no connected
  provider has offered for 15 minutes (mediaconv down, or missing a tool).
- **Tools:** mediaconv's `ffmpeg` and ImageMagick (animated stickers to
  WhatsApp), and the Go WhatsApp connector's `webpmux` (sticker pack names).
- **Pins:** an account that may not pin where pins sync (`chat_state`).

A failed chat check **pauses** that rule's direction into the chat. Its
messages are held, not sent, and when a later check passes they go out in
order. A message held for more than 7 days is dropped, and the ✅ notice
says how many. A pause covers one direction only: an admin-only WhatsApp
group holds replies from the topic, while the group still mirrors into it.
Everything else only warns.

You hear about each problem once, in the `notify` chats, and again with a
✅ when it is fixed. When a connector cannot tell (it is not connected, or
the lookup failed), nothing changes: an unknown never pauses or resumes.

```sh
wuphf-server health --config FILE          # the open problems, from the database
wuphf-server health --config FILE --run    # check now (WUPHF_OBSERVER_TOKEN)
```

Both exit 1 while a problem that is not ignored is open. To ignore a check
you know is wrong, use the Health page in `/wuphf` (it lapses once the check
passes).

## Restarts and recovery

- The core keeps every pending delivery in its database. A restart, or a
  connector that is down, loses nothing: deliveries resume when the connector
  reconnects.
- Connectors reconnect to the core by themselves.
- After a WhatsApp logout, the connector links again by itself and sends you
  the code. Messages for WhatsApp wait in the core meanwhile.
- Database migrations run when the core starts.
