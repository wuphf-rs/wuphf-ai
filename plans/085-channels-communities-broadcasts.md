# Plan 085: WhatsApp channels, communities and broadcast lists

> **Executor instructions**: as plan 080.

## Status

- **Priority**: P2
- **Effort**: L
- **Depends on**: 083, 084 (posting flow), 086 (footer edits on long posts)
- **Category**: core / storage / settings / connectors (Telegram, both WhatsApp)
- **State**: DONE 2026-11-26 (see the notes in `plans/README.md`); live test
  pending the owner.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Followed channels are mirrored**, one topic each, receive-only (083); post edits and deletions update the Telegram copy. |
| 2 | **Channels you own or admin** take posts like stories (084): plain messages are refused; `/post`, `/post!`, the panel's compose page; preview + confirm; owner ids only. Edits of your channel posts go through without re-confirming. Deletes follow 083's toggle. |
| 3 | **Reaction counts** on channel posts: a footer line on the Telegram copy (`❤️ 120 · 👍 34`), edited only when counts change. The core polls each channel for posts whose counts changed since its last poll every `channels.counts_poll_minutes` (default 15); posts older than `channels.counts_max_age_days` (default 30) stop updating. No live subscriptions. |
| 4 | **Counts stay generic in the core**: a reaction summary per logical message (new table, migration), a poll schedule for chats whose connector reports summaries, diff and coalescing. |
| 5 | **Communities**: subgroup topics are titled `[G] Community › Group`; the announcement group is receive-only unless you're admin (083's facts); a community can be picked for a rule's chats and for `receive_only`, and covers its current and future subgroups; the info card names the community. |
| 6 | **Broadcast lists**: a broadcast someone sent you shows in their DM topic with an inline `[broadcast]` tag after the sender name. Your own phone-sent broadcast shows in the DM topic of each person it went to. Telegram never sends to a broadcast list. |

Planner decisions (2026-11-26; change them in review if wrong):

| # | Decision |
|---|---|
| 7 | **Posting facts for channels**: owner/admin → `{messages: false, posts: true, post_edits: true}`; follower → receive-only. `ChatPosting.post_edits` (new) lets 083's screen pass an edit of a message already posted there; Status keeps it off (WhatsApp cannot edit a story). |
| 8 | **Channel reactions from Telegram are not sent** (WhatsApp addresses them by a server id the bridge does not keep): channels report `reactions: false`, and the screen drops them. |
| 9 | **Only whatsapp-main posts to channels and reports counts** (as 084 for stories); whatsapp-rust mirrors channels, communities and broadcast lists. |
| 10 | **The footer is the core's**: the core composes `❤️ 120 · 👍 34` and sends it as an edit of the copy (`MessageEditDispatch` with the text and the footer), so 086's edit path renders it; a later edit of the post keeps the footer. |
| 11 | **Polling is a request, not a dispatch**: `RefreshReactions { request_id, chat_id, since_ms }` from the core; the connector answers with `ReactionSummaryEvent`s (one per post whose counts it read). |
| 12 | **The info card names the community only** (not its other groups). |

## Steps

1. Broadcast lists (both WhatsApp connectors): incoming broadcasts go to the
   sender's DM with `[broadcast]` after the sender name; own broadcasts go to
   each recipient's DM. Tests.
2. Channels (whatsapp-main): posting facts with `post_edits`; explicit posts
   to an owned channel (text; media through `UploadNewsletter`); the
   backstop refuses non-post sends to channels; the screen's `post_edits`.
   Tests.
3. Communities: `ChatProfile.community` from the connectors (snapshot and
   group info); titles `[G] Community › Group`; rule chat filters and
   `receive_only` match a community's subgroups; the info card line. Tests.
4. Reaction counts: proto (`ReactionSummaryEvent`, `RefreshReactions`,
   `ChatPosting.summaries`), model, storage (migration), the core's diff and
   footer edits, the runtime's poll timer and `[channels]` settings,
   whatsapp-main's `GetNewsletterMessageUpdates`. Tests.
5. Docs, notes, dev bins.

## Live test (owner)

1. A broadcast-list message from someone lands in their DM topic as
   `Name [broadcast]`; your own phone broadcast shows in each recipient's.
2. A followed channel's topic: typing there gets 👎 and a note; a new post
   and its edit arrive; within 15 minutes a post with reactions shows
   `❤️ n · 👍 m` under it, and changes when they do.
3. A channel you own: `/post! hello` in its topic posts; a photo posted with
   `/post` (preview, ✅) arrives with its caption; editing your message
   edits the post.
4. A community: its groups' topics read `[G] Community › Group`; the info
   card of a new one names it; `only_chats` with the community's id mirrors
   a group added to it later.
