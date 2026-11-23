# Plan 062: The chat directory, and chats picked by name in setup

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `CARGO_BUILD_JOBS=2 nice -n 19` / `GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19`,
> only the crates and modules touched. Stage explicit paths only.

## Status

- **Priority**: P1 — the owner asked for it (2026-11-23)
- **Effort**: M
- **Depends on**: botconf plan 017 (`v0.14.0`: domains and suggestions), plans
  059 and 060
- **Category**: feature / DX

## Why this matters

Rules, `notify`, chat-state overrides and misc's `[instagram] chats` hold
chats as raw `connector:chat` ids, typed by hand in setup. The owner wants
them picked by name (2026-11-23), from chat lists the connectors fetch when
their account is linked in setup (plan 063), so a first setup needs nothing
running.

## Design decisions (settled with the owner, 2026-11-23)

1. **The chat directory**: one file per connector, `<connector_id>.chats.json`,
   in `$WUPHF_CHAT_DIRECTORY`, else `$XDG_STATE_HOME/wuphf/chats` (else
   `~/.local/state/wuphf/chats`). Written whole (temporary file, rename) by a
   connector's setup; read by the core's and misc's. Format, versioned:
   `{"version": 1, "connector_id", "platform", "fetched_at_ms", "chats":
   [{"chat": "<connector:chat>", "title", "kind": "group" | "direct" |
   "channel"}]}`; the connector writes the ids as it normalizes them for the
   core, so they are the ids rules match.
2. **Pickers**: the chat options carry the `wuphf.chat` domain (`notify`,
   rules' `source`, `destination`, `only_chats`, `except_chats`, chat-state
   overrides' `chat`, misc's `instagram.chats`), connector ids `wuphf.connector`
   (`source_connector`). The core's setup suggests chats from every directory
   file of a connector its config being edited lists (title first, then the
   id), and connectors from that config; misc's suggests chats the same way.
   The core's panel keeps searching the chats it has observed; the derived
   reference kinds replace the hand-written `panel = Kind::reference(…)`.
3. With no directory file, the editor says how to get one ("link the
   connector in its setup: wuphf-<platform> setup → Link account").

## Steps

1. The format: Rust types and the file's path in `wuphf-model`
   (`ChatDirectory`, with tests), Go in `connectors/shared/chatdir` (read,
   write, the path), and a shared fixture both decode.
2. The core: pins to botconf `v0.14.0`; the domains on the config types; the
   setup's `suggest`; tests (suggestions from a temp directory, unknown
   connectors left out).
3. misc: the domain on `instagram.chats`, its `suggest`; tests.
4. Docs (README "First-time setup": the directory), gate, dev binaries, row
   062 DONE.
