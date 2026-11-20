# systemd units

System units for every wuphf part, grouped under `wuphf.target`:

| Unit | Binary | Config | Notes |
|---|---|---|---|
| `wuphf-core.service` | `wuphf-server` (the `wuphf-cli` binary) | `core.toml` | Start this first; the others connect to it. |
| `wuphf-telegram.service` | `wuphf-telegram` | `telegram.toml` | A user session logs in interactively once. |
| `wuphf-whatsapp.service` | `wuphf-whatsapp` | `whatsapp.toml` | Pairs by QR code once. |
| `wuphf-whatsapp-rust.service` | `wuphf-whatsapp-rust` | `whatsapp-rust.toml` | Opt-in second WhatsApp connector; pairs once. |
| `wuphf-instagram.service` | `wuphf-instagram` | `instagram.toml` | Needs the exported cookies file. |
| `wuphf-mediaconv.service` | `wuphf-mediaconv` | `mediaconv.toml` | Needs `ffmpeg` and ImageMagick on `PATH`. |
| `wuphf-misc.service` | `wuphf-misc` | `misc.toml` | Holds the observer token (full access); see `misc/README.md`. |

Enable only the parts your `core.toml` configures.

## Layout

- Binaries in `/usr/local/bin`, named as `scripts/install-dev-bins.sh` names
  them (it builds them all into `scripts/dev-bin/.bin/`).
- Config in `/etc/wuphf/<part>.toml`, started from each part's
  `config.example.toml` (`config/example.toml` for the core).
- Secrets in `/etc/wuphf/<part>.env` (templates in `env/`), mode `0600`,
  owner root: systemd reads them, the `wuphf` user cannot.
- State in `/var/lib/wuphf/<part>` (systemd creates it, owned by `wuphf`). It is
  each unit's working directory, so the relative paths in the example configs
  (`session_file`, `session_database`, `sqlite://wuphf.sqlite`) work unchanged.

## Install

```sh
# 1. A system user for every part.
sudo useradd --system --home-dir /var/lib/wuphf --shell /usr/sbin/nologin wuphf

# 2. Binaries (built with scripts/install-dev-bins.sh).
sudo install -m 0755 scripts/dev-bin/.bin/wuphf-* /usr/local/bin/

# 3. Config and secrets. Copy each example, then edit it.
sudo install -d -m 0750 -o root -g wuphf /etc/wuphf
sudo install -m 0640 -o root -g wuphf config/example.toml /etc/wuphf/core.toml
sudo install -m 0640 -o root -g wuphf connectors/telegram/config.example.toml /etc/wuphf/telegram.toml
# ... the same for whatsapp, whatsapp-rust, instagram, providers/mediaconv and misc
for part in core telegram whatsapp instagram mediaconv misc; do
    sudo install -m 0600 -o root -g root deploy/systemd/env/$part.env.example /etc/wuphf/$part.env
done
# Fill the tokens: one random value per part, the same in core.env and in
# that part's own file (`openssl rand -hex 32`). wuphf-misc's token is the
# core's observer token (WUPHF_CORE_OBSERVER_TOKEN in core.env).

# 4. Units.
sudo install -m 0644 deploy/systemd/wuphf.target deploy/systemd/wuphf-*.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable wuphf.target wuphf-core.service wuphf-telegram.service \
    wuphf-whatsapp.service wuphf-mediaconv.service
```

Do the first logins (below) before you start the connectors that need one.
Then:

```sh
sudo systemctl start wuphf.target
journalctl -fu 'wuphf-*'
```

## First login

These steps need a terminal. `systemd-run --pty` runs the binary as `wuphf`, in
its state directory, with its env file, exactly as the unit will:

```sh
first_run() { # part binary config-flag
    sudo systemd-run --pty --wait --collect \
        -p User=wuphf -p Group=wuphf \
        -p StateDirectory=wuphf/$1 -p WorkingDirectory=/var/lib/wuphf/$1 \
        -p EnvironmentFile=/etc/wuphf/$1.env \
        /usr/local/bin/$2 $3 /etc/wuphf/$1.toml
}
```

- **WhatsApp** (`first_run whatsapp wuphf-whatsapp -config`): scan the QR code
  it prints, wait for "WhatsApp session connected", then stop it with Ctrl-C.
  `wuphf-whatsapp-rust --config` works the same way.
- **Telegram user session** (`first_run telegram wuphf-telegram -config`): type
  the login code (and the 2FA password when asked), wait until it connects,
  then Ctrl-C. A bot session needs no first run.
- **Instagram**: no interactive step. Put the cookies file that `cookies_file`
  names into `/var/lib/wuphf/instagram/` (owner `wuphf`, mode `0600`) before
  the first start. The connector writes refreshed cookies to `session_file` on
  shutdown.

The core must be running for a connector to connect, but pairing itself does
not need it.

## Notes

- **Stop signal.** The connectors and the provider shut down cleanly (saving
  their session) only on SIGINT, so their units set `KillSignal=SIGINT`. The
  core keeps the default SIGTERM: every write is a SQLite transaction.
- **Order.** Connectors start after the core and reconnect to it with backoff
  on their own; `Restart=on-failure` covers a process that exits.
- **Hardening.** Each unit runs read-only outside its state directory
  (`ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, …).
  `wuphf-mediaconv` leaves out `MemoryDenyWriteExecute` because it runs
  ffmpeg and ImageMagick.
- **wuphf-misc.** It needs no first run. The Instagram downloader fetches
  through the Instagram connector: enable that connector's `[functions]`
  section (and `WUPHF_PROVIDER_INSTAGRAM_TOKEN` in `instagram.env` and
  `core.env`) and the core's matching `[[functions.providers]]` entry.
- **Remote core.** When a connector runs on another host, drop
  `wuphf-core.service` from its `After=` and point its `core_address` at the
  core.
- **Upgrades.** Install the new binaries, then
  `sudo systemctl restart wuphf.target`. The core applies its SQLite
  migrations on start.
- **TUI.** `wuphf-tui` is interactive and has no unit:
  `wuphf-tui --server http://127.0.0.1:50051 --token <observer_token>`.
