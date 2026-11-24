# Plan 066: A Docker image of the release binaries, a compose setup, and GHCR

> **Executor instructions**: as plan 058. An image build only downloads and
> installs packages; build one at a time all the same.

## Status

- **Priority**: P2 — the owner asked for it (2026-11-24)
- **Effort**: M
- **Depends on**: 064 (signed releases), 065 (the deployment layout it mirrors)
- **Category**: packaging / ops

## Why this matters

wuphf installs from releases (systemd) or the flake (NixOS). The owner wants
a Docker setup too (2026-11-24).

## Design decisions (settled with the owner, 2026-11-24)

1. **From the releases, not from source.** `deploy/docker/Dockerfile` downloads
   one release's binaries for the build's architecture (`ARG VERSION`,
   `TARGETARCH` → `x86_64`/`aarch64`). It installs them only after checking
   `SHA256SUMS.minisig` against `release/minisign.pub`, the trusted comment
   (`wuphf vX.Y.Z`) and every sum, the same checks `wuphf-server update` does. The build logic stays in
   `scripts/build-release.sh`; the image only packages it.
2. **One image, every binary** (`ghcr.io/wuphf-rs/wuphf`). Each compose service
   picks its binary. The base is `debian:trixie-slim`: glibc ≥ 2.35, ImageMagick 7
   for animated WebP. It also holds ffmpeg, ImageMagick and `webp` (webpmux,
   for WhatsApp sticker metadata), CA certificates and tini.
3. **Layout as the systemd units'.** Config and env files are in `/etc/wuphf`,
   with one state directory per part in `/var/lib/wuphf/<part>`, which is also
   the working directory, so relative paths work unchanged. The chat directory
   is `/var/lib/wuphf/chats` (`WUPHF_CHAT_DIRECTORY`). The core's release check
   is off (`WUPHF_CORE_UPDATES__CHECK_INTERVAL_SECONDS=0`): an image is updated
   by pulling the next one.
4. **A non-root user, chosen at run time.** The entrypoint starts as root and
   gives each mounted state directory to `PUID:PGID`, `chown -R` only when the
   owner differs. It then drops to that user with `setpriv`, clearing every
   capability. Files that setup writes into the bind-mounted config directory
   belong to the host user.
5. **Compose** (`deploy/docker/compose.yaml`). `core` always runs. Each other part is a
   profile (`COMPOSE_PROFILES` in `.env`). Everything is in two host
   directories that people mount (the owner asked for this): `WUPHF_DATA`
   (`./data`) holds `<part>/` per part and the shared `chats/`, and
   `WUPHF_CONFIG` (`./config`) is mounted read-write at `/etc/wuphf`, because
   setup writes it. The env files are optional `env_file`s.
   The core listens on `0.0.0.0:50051` inside the network (`WUPHF_CORE_LISTEN`)
   and is published on the host's loopback only. The connectors stop on SIGINT
   and misc gets 90s, as in the units. Hardening: read-only root,
   tmpfs `/tmp`, `no-new-privileges`, and every capability dropped except
   the ones the entrypoint needs before it drops them.
6. **GHCR on release tags.** `.github/workflows/docker.yml` is reusable: it is
   called by `release.yml` after `publish`, or run by hand with a version. It
   builds `linux/amd64` and `linux/arm64` (QEMU, for the arm64 `apt-get`) and
   pushes `X.Y.Z`, `X.Y` and `latest` (no `latest` for a prerelease).

## Verification

- `docker build` of v0.2.0 for amd64: every binary reports `0.2.0`. A wrong
  `minisign.pub` fails the build.
- `magick`, `ffmpeg` and `webpmux` run in the image. ImageMagick decodes an
  animated WebP.
- Compose: the core starts on an empty config directory and listens. A
  connector reaches it at `core:50051`. State lands in `data/<part>`, owned by
  `PUID`, including the directories Docker creates as root.
- `actionlint`-clean workflows (when it's available).

## Owner steps

- After the first push, make the `wuphf` package public on GHCR
  (organization → Packages → wuphf → Package settings).
