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

The flake has every binary as a package and a NixOS module. Each part is a
service, with the same user, state directories and hardening as the systemd
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
            core = {
              enable = true;
              settings = {            # config/example.toml, as Nix
                listen = "127.0.0.1:50051";
                database_url = "sqlite://wuphf.sqlite";
                notify = [ "telegram-main:telegram:user:704338780" ];
                connectors = [ /* … */ ];
                rules = { /* … */ };
              };
              environmentFile = "/run/secrets/wuphf-core.env";  # the tokens
            };
            telegram = {
              enable = true;
              settings = { /* connectors/telegram/config.example.toml */ };
              environmentFile = "/run/secrets/wuphf-telegram.env";
            };
            whatsapp.enable = true;    # also: whatsapp-rust, instagram, mediaconv, misc
          };
        }
      ];
    };
  };
}
```

- Every part has `enable`, `settings` (its TOML file as Nix) or `configFile`,
  `environmentFile` (its tokens, kept out of the store; sops-nix or agenix
  work), and `package`.
- The core's `observer_token` is a value, not a variable name: leave it out
  of `settings` (the store is world-readable) and put
  `WUPHF_CORE_OBSERVER_TOKEN=…` in its `environmentFile` instead.
- State is in `/var/lib/wuphf/<part>`, as on any server; relative paths in
  the settings resolve there.
- First logins: stop the part, then run its `setup` (Link account) or run it
  once by hand as `wuphf` in its state directory, as the
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

## Restarts and recovery

- The core keeps every pending delivery in its database. A restart, or a
  connector that is down, loses nothing: deliveries resume when the connector
  reconnects.
- Connectors reconnect to the core by themselves.
- After a WhatsApp logout, the connector links again by itself and sends you
  the code. Messages for WhatsApp wait in the core meanwhile.
- Database migrations run when the core starts.
