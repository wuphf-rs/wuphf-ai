# Plan 060: Offline setup for the Go binaries, on botconf's config descriptions

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19`, only the modules touched. Stage
> explicit paths only.

## Status

- **Priority**: P1 — the owner asked for it (2026-11-23)
- **Effort**: L
- **Depends on**: botconf plan 015 (`go/v0.13.0`: `go/describe`, `go/tui`),
  plan 059 (the conventions)
- **Category**: feature / DX

## Why this matters

See plan 059: the Telegram, WhatsApp, Instagram, mediaconv and misc
binaries get the same `setup` subcommand, each option described once on
its config struct.

## Design decisions

- Every config field carries its description in tags (`doc`, `title`,
  `default`, `env`, `secret`; `botconf` still marks runtime keys), moved
  from the Go comments; botconf's `go/describe` reflects them. wuphf's
  `connectors/shared/settings` declares runtime settings from that
  description (its own reflection goes).
- `config.example.toml` is generated per binary, with a drift test.
- `<binary> setup` runs `go/tui` with the binary's own loader as the
  validator.

## Steps

1. Pins to `go/v0.13.0`; `connectors/shared/settings` on `go/describe`.
2. Telegram, WhatsApp, Instagram, mediaconv, misc: tags, generated example,
   drift test, `setup`.
3. Docs, gate, dev binaries, row 060 DONE.
