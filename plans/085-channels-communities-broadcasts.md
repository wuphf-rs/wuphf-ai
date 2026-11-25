# Plan 085: WhatsApp channels, communities and broadcast lists

> **Executor instructions**: as plan 080.

## Status

- **Priority**: P2
- **Effort**: L
- **Depends on**: 083, 084 (posting flow), 086 (footer edits on long posts)
- **Category**: core / storage / settings / connectors (Telegram, both WhatsApp)

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Followed channels are mirrored**, one topic each, receive-only (083), reactions allowed; post edits and deletions update the Telegram copy. |
| 2 | **Channels you own or admin** take posts like stories (084): plain messages are refused with a note carrying a "Post to channel" button; `/post`, `/post!`, panel; preview + confirm; owner ids only. Edits of your channel posts go through without re-confirming. Deletes follow 083's toggle. |
| 3 | **Reaction counts** on channel posts: a footer line on the Telegram copy (`❤️ 120 · 👍 34`), edited only when counts change. The core polls each channel for posts whose counts changed since its last poll every `channels.counts_poll_minutes` (default 15); posts older than `channels.counts_max_age_days` (default 30) stop updating. Telegram edits are budgeted per chat and wait out FLOOD_WAIT. No live subscriptions (the phone holds them only while a channel is open). |
| 4 | **Counts stay generic in the core**: a reaction summary per logical message (new table, migration), a poll schedule for chats whose connector declares the capability, diff and coalescing. WhatsApp maps a poll to `GetNewsletterMessageUpdates(since)` (whatsmeow) or re-reading recent posts (`get_messages`, whatsapp-rust). |
| 5 | **Communities**: subgroup topics are titled `[G] Community › Group`; the announcement group is receive-only unless you're admin (083's facts); a community can be picked in `/wuphf` chat pickers and covers its current and future subgroups; the info card names the community and its groups. |
| 6 | **Broadcast lists**: a broadcast someone sent you shows in their DM topic with an inline `[broadcast]` tag after the sender name. Your own phone-sent broadcast shows only in the DM topics of the people it was delivered to. Telegram never sends to a broadcast list. |

## Steps

1. Channels: mirror, edits/deletes, owned-channel posting via 084's flow.
2. Reaction summaries: model, storage (migration), core poll schedule,
   WhatsApp polls, Telegram footer under an edit budget.
3. Communities: titles, pickers (live membership), info card.
4. Broadcast lists: tag, own broadcasts per recipient.
5. Docs, notes, dev bins.
