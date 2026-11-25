# Plan 086: Edits that no longer fit as text swap the Telegram copy in place

> **Executor instructions**: as plan 080.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Depends on**: — (lands before 085)
- **Category**: Telegram connector

## Why

`Sender.EditMessage` always edits text (`Edit(id).StyledText`). A message
that was sent as a `.txt` file because it was too long, a short message
edited past the length limit, or a caption edited past 1024 characters cannot
be edited that way: the edit fails or comes out wrong.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Swap in place, every topic**, keeping the message id: a spilled copy gets a new `.txt` with the edited text (`messages.editMessage` with new media); text that became too long turns into a `.txt`; an over-long caption is trimmed and the full text goes into a `.txt` reply. |
| 2 | Footer edits (085's reaction counts) go through this path. |

## Steps

1. Telegram connector: know how a copy was rendered (spilled / captioned /
   text) from the message itself (`messages.getMessages`) or a local record.
2. Edit paths for each case; tests with the fake sender.
3. Docs, notes, dev bins.
