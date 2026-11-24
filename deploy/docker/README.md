# Docker

One image, `ghcr.io/wuphf-rs/wuphf`, holds every binary of one release:
`wuphf-server`, `wuphf-tui`, the connectors, `wuphf-mediaconv` (with ffmpeg
and ImageMagick) and `wuphf-misc`. `compose.yaml` runs each part as its own
service, as the [systemd units](../systemd/README.md) do.

| Tag | What |
|---|---|
| `0.2.0` | that release |
| `0.2` | the newest 0.2.x |
| `latest` | the newest release |

Images are for `linux/amd64` and `linux/arm64`. They hold the release's own
binaries, installed only after `SHA256SUMS` was checked against wuphf's
release key, the same checks `wuphf-server update` makes.

## Layout

Everything lives in two directories on the host, next to `compose.yaml`
unless `.env` says otherwise:

| Host | In the container | What |
|---|---|---|
| `config/` (`WUPHF_CONFIG`) | `/etc/wuphf` | `<part>.toml` and `<part>.env` for every part |
| `data/<part>/` (`WUPHF_DATA`) | `/var/lib/wuphf/<part>` | the part's session, database and files; its working directory |
| `data/chats/` | `/var/lib/wuphf/chats` | the chat directory: the chats each connector's setup fetched, offered by name in the other setups |

Relative paths in the example configs (`sqlite://wuphf.sqlite`,
`session_file`, `session_database`) therefore land in `data/<part>/`.

Every part runs as `PUID:PGID` from `.env` (your own user). On each start,
the entrypoint gives the part's data directories to that user, including
those Docker just created, then drops every privilege. The files are then
yours to read, copy and back up.

## Install

```sh
cd deploy/docker
cp .env.example .env            # WUPHF_VERSION, COMPOSE_PROFILES, PUID/PGID (`id -u`, `id -g`)
mkdir -p config data
```

`COMPOSE_PROFILES` names the parts that run next to the core: `telegram`,
`whatsapp`, `whatsapp-rust`, `instagram`, `mediaconv`, `misc`.

### Configure

Each part's setup screen writes its `config/<part>.toml` and the tokens into
`config/<part>.env`. Run the core's first:

```sh
docker compose run --rm -it --no-deps core \
    wuphf-server setup --config /etc/wuphf/core.toml --env-file /etc/wuphf/core.env
```

[Getting started](../../docs/wiki/Getting-Started.md#3-configure-the-core)
says what to set. Differences from a server install:

- **`listen`** is `0.0.0.0:50051`. compose sets it anyway (`WUPHF_CORE_LISTEN`)
  so the other parts can reach the core. It is published on the host's
  loopback only.
- **Set the tokens that protect a listening core.** Other containers can
  reach it, so set `observer_token` (in `core.env`:
  `WUPHF_CORE_OBSERVER_TOKEN=…`) and, with providers, `[functions]
  invoke_token_env`.
- **`core_address = "core:50051"`** in every other part's config, the
  core's service name.

Copying the example files works too: `config/example.toml` for the core,
`<part>/config.example.toml` for the others, with the env templates in
[`../systemd/env/`](../systemd/env).

### Link the accounts

Run each connector's setup in its own container, so the session lands in
`data/<part>/` and its chats in `data/chats/`:

```sh
docker compose run --rm -it telegram \
    wuphf-telegram setup --config /etc/wuphf/telegram.toml --env-file /etc/wuphf/telegram.env
docker compose run --rm -it whatsapp \
    wuphf-whatsapp setup --config /etc/wuphf/whatsapp.toml --env-file /etc/wuphf/whatsapp.env
```

Then **Link account**. For WhatsApp, scan the QR code in WhatsApp → Linked
devices. A Telegram user session asks for its login code. `wuphf-whatsapp-rust`
links the same way. For Instagram, put the cookies file that `cookies_file`
names into `data/instagram/`.

Linking needs no running core. Afterwards, the core's setup offers those
chats by name for rules and notices.

## Run

```sh
docker compose up -d
docker compose logs -f
```

The connectors stop on SIGINT so they save their sessions (`stop_signal`),
and wuphf-misc gets 90 seconds to finish. Each container's root is
read-only, and the part runs with no capabilities.

- **A config or env file changed:** `docker compose up -d` recreates the
  parts whose env changed. For a changed `.toml`, run `docker compose
  restart <part>`.
- **Runtime settings** (`/wuphf` in Telegram, or the CLI) need no restart:

  ```sh
  docker compose exec core wuphf-server settings --config /etc/wuphf/core.toml list
  ```

- **The TUI:** `docker compose exec core wuphf-tui --server http://127.0.0.1:50051 --token <observer_token>`

## Updates

The core's release check is off in the image, and `wuphf-server update`
cannot replace its binaries. Update by pulling the next image:

```sh
# .env: WUPHF_VERSION=0.3.0 (or keep `latest`)
docker compose pull
docker compose up -d
```

The core applies its database migrations when it starts. To go back, set the
previous version and run `up -d` again. A migration is not undone, so back
up `data/core/` before an update.

## Backups

`config/` and `data/` hold everything. Back them up with the parts stopped:

```sh
docker compose stop
tar czf wuphf-backup.tgz config data
docker compose start
```

With the core running, one TOML file holds the settings with every stored
override and no secrets
([Operations](../../docs/wiki/Operations.md#backups)):

```sh
docker compose exec core sh -c \
    'WUPHF_OBSERVER_TOKEN=$WUPHF_CORE_OBSERVER_TOKEN wuphf-server settings export' > settings.toml
```

## Building the image

The Dockerfile packages a published release; it compiles nothing. From the
repository's root:

```sh
docker build -f deploy/docker/Dockerfile --build-arg VERSION=0.2.0 \
    -t ghcr.io/wuphf-rs/wuphf:0.2.0 .
```

`--platform linux/arm64` builds the arm64 image (under QEMU on an x86_64
host). A release tag pushes both images through
`.github/workflows/docker.yml`. Running that workflow by hand rebuilds the
image of a release that is already published.
