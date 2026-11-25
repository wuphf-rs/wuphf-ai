# Plan 084: Posting your own WhatsApp stories from Telegram; story mutes and expiry

> **Executor instructions**: as plan 080.

## Status

- **Priority**: P2
- **Effort**: L
- **Depends on**: 083
- **Category**: core / settings / connectors (Telegram, both WhatsApp)

## Why

083 makes the Status topic receive-only, so nothing reaches
`status@broadcast` by accident. The owner still wants to post stories from
Telegram, deliberately, and to stop mirroring stories from some people.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Real stories** (to `status@broadcast`), not watgbridge's About text with expiry (`/setstatus`, out of scope). |
| 2 | **Starting a post**: reply `/status` to a message in a connector's Status topic, `/status <text>`, the Post button on 083's refusal note (in the Status topic the note carries it and stays until used, at most 10 min), or the panel's "Post status" (asks for the connector when there are several). |
| 3 | **Content**: text, photo, video, voice/audio, a link with its preview. |
| 4 | **Confirmation**: a preview with Post / Cancel, naming the WhatsApp account and the audience the phone's status privacy gives; previews expire. `/status!` skips it. |
| 5 | **Only owner ids** may post. Anyone else gets nothing. |
| 6 | **Edits** of a posted story are refused (👎 + note): WhatsApp can't edit stories. **Deletes** follow 083's per-connector toggle. |
| 7 | **Mutes**: `status.mirror` (all stories on/off) and `status.muted` (people whose stories are skipped), runtime settings in `/wuphf`. Muted stories are dropped entirely. |
| 8 | **Expiry**: `status.expire_copies` runtime setting, default off (keep forever); on, Telegram story copies are deleted 24 h after posting. |

## Design (outline)

- The core owns the post: a `PostStatus` request (from the Telegram
  connector's command or button, checked against `owner_ids`) becomes a
  dispatch to the source connector flagged as an explicit post
  (`MessageCreateDispatch.post = true`), the only flag 083's backstop lets
  through to `status@broadcast`.
- The preview is a pending post stored in the core (expires after 10 min),
  rendered by the Telegram connector with inline buttons (helper bot for user
  sessions, as 043).
- The audience line comes from the WhatsApp connector (`GetStatusPrivacy`)
  through a probe-like request.
- Mutes are applied at ingest (core): a status message whose author is in
  `status.muted` (any address form) or with `status.mirror` off is
  dropped before storage. The core knows a chat is a story feed from
  083's `posting.private_replies`.
- Expiry: the core schedules deletes of the mirror copies of feed messages.

## Steps

1. Proto/model: `post` flag, `PostStatus` request, pending posts (migration).
2. Core: post flow, owner check, preview expiry, mutes, expiry scheduling.
3. WhatsApp (both): explicit post to `status@broadcast` (text with
   background, media, voice, link preview); status privacy lookup.
4. Telegram: `/status`, `/status!`, note button, panel entry, preview card.
5. Docs, notes, dev bins.
