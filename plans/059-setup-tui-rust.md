# Plan 059: Offline setup for the Rust binaries (the core, whatsapp-rust), on botconf's config descriptions

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `CARGO_BUILD_JOBS=2 nice -n 19`, `-p` the crates touched; whatsapp-rust
> needs `RUSTC_BOOTSTRAP=1` and runs alone. Stage explicit paths only.

## Status

- **Priority**: P1 — the owner asked for it (2026-11-23)
- **Effort**: L
- **Depends on**: botconf plan 015 (`v0.13.0`: `Describe`, the generated
  example, `botconf-tui`)
- **Category**: feature / DX

## Why this matters

First-time setup and later changes mean editing TOML whose options are
documented only in comments, plus `.env.local` for the tokens they name.
The owner wants every binary to open a TUI listing all its options with
their descriptions, writing its config offline. Each option must be
described **once**: on the config type (owner, 2026-11-23), from which
botconf derives the runtime settings, the example and the TUI.

## Design decisions (settled with the owner, 2026-11-23)

1. The config types derive `botconf::Describe`; their doc comments are the
   descriptions. The core's hand-written runtime descriptors (`schema.rs`
   consts) are replaced by the derived ones where the derive can express
   them (kind overrides where it cannot); whatsapp-rust's `RuntimeSettings`
   list is replaced by its derived description.
2. `config.example.toml` / `config/example.toml` are generated, with a drift
   test each.
3. `wuphf-server setup` and `wuphf-connector-whatsapp setup` (`--config`,
   default `config.local.toml`; `--env-file`, default `.env.local`) run
   `botconf-tui`, validating with the binary's own loader.
4. Secrets: env-variable options set their values in the env file; token
   variables can be generated; the core's and the connectors' token
   variables share names, so one value serves both.
5. wuphf-tui has no config file (flags only): no `setup`.

## Steps

1. Pins to botconf `v0.13.0`; the core's `Configuration` (and nested types)
   derive `Describe`; runtime settings from the description; the drift test;
   `wuphf-server setup`.
2. whatsapp-rust: `Config` derives `Describe` (the SDK's `RuntimeSettings`
   takes a description: plan 056's list goes); the drift test; `setup`.
3. Docs (README "First-time setup"), dev aliases, gate, dev binaries, row
   059 DONE.
