# Plan 076: Pause and resume bridging for one chat, as the `paused_chats` runtime setting

> **Executor instructions**: as plan 058.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Depends on**: 074 (audit and notices). The panel's buttons come in 075.
- **Category**: core / settings

## Design decisions (settled with the owner, 2026-11-24)

1. A chat can be paused without editing its rule. Messages sent while it is
   paused are **never bridged**: they are not stored as pending, and resuming
   bridges only new messages.
2. The pause is a **runtime setting**, `paused_chats`: a list of chats in
   `connector:chat` form, in the `wuphf.chat` domain. That puts it in
   `/wuphf` settings, the CLI and settings export, and it applies live like
   rules.

## Design

- The router checks `paused_chats` before rules, for both the source and
  every target. It pauses both directions of a mirror: a reply typed in a
  paused chat's topic is not bridged either. An ingested event from a paused
  chat is still recorded for dedup, so a later resume does not replay it.
  It's acknowledged as `Ignored(Paused)`.
- **Panel (075):** a Pause / Resume toggle on the conversation page, and the
  rule's page lists its paused chats. Both are audited, with a notice.
- **The info card and notes are paused too.** A profile change while paused
  is stored silently and posts no note.

## Steps

1. The setting in `wuphf-settings` (schema, description, example) and the
   botconf domain.
2. The router and ingest checks, with `IgnoreReason::Paused`; core tests:
   - nothing is dispatched while paused, either way;
   - resuming bridges only new messages;
   - a paused chat posts no notes.
3. The panel toggle and its audit and notice (075's page).
4. Docs: Configuration (Runtime settings), architecture (Routing Rules).
