# Plan 061: Back up and restore the settings: `wuphf-server export` / `import`, online `wuphf settings export` / `import`

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `CARGO_BUILD_JOBS=2 nice -n 19`, `-p` the crates touched. Stage explicit
> paths only.

## Status

- **Priority**: P1 — the owner asked for it (2026-11-23)
- **Effort**: M
- **Depends on**: botconf plan 016 (`botconf::export`, the `Export` /
  `Import` RPCs), plan 059 (the core's description and `setup`)
- **Category**: feature / ops

## Why this matters

The core's settings are its config file plus the overrides in its SQLite
database: its own runtime keys and every connector's
(`connector_settings.<id>`). The owner wants one backup of all of it, and a
restore (2026-11-23).

## Design decisions (settled with the owner, 2026-11-23)

1. **The core's backup covers every binary's runtime overrides**: the core
   stores the connectors' too, so the connectors get no `export` of their
   own (their file is their whole base; back it up as a file).
2. `wuphf-server export [-o FILE] [--with-secrets]` and `wuphf-server import
   FILE` (`--config`, default `config.local.toml`; `--env-file`, default
   `.env.local`) run offline on the database the config names; import
   refuses while a server holds the database (its lock), validates with the
   core's own loader, and writes `.bak` copies first.
3. `wuphf settings export` / `import` go through the running core (botconf's
   `Export` / `Import`): never secrets; import applies what can change at
   runtime and lists the rest as needing the offline import.
4. The setup TUI (`wuphf-server setup`) has Export and Import.

## Steps

1. Pins to the botconf release with plan 016; `wuphf-server export` /
   `import` on `botconf::export` (the core's description, base figment,
   `SettingsStorage` over SQLite, the `connector_settings` namespace).
   Tests: round trip on a temp database; import refused on an invalid backup
   writes nothing; secrets only with the flag.
2. The core serves `Export` / `Import` (owner-only, as `Set`); `wuphf
   settings export` / `import`. Tests on the service.
3. Docs (README "Backups"), gate, dev binaries, row 061 DONE.
