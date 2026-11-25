# Plan 084: Posting your own WhatsApp stories from Telegram; story mutes and expiry

> **Executor instructions**: as plan 080.

## Status

- **Priority**: P2
- **Effort**: L
- **Depends on**: 083
- **Category**: core / settings / runtime (panel pages) / connectors (Telegram, both WhatsApp)

## Why

083 makes the Status topic receive-only, so nothing reaches
`status@broadcast` by accident. The owner still wants to post stories from
Telegram, deliberately, and to stop mirroring stories from some people.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Real stories** (to `status@broadcast`), not watgbridge's About text with expiry (`/setstatus`, out of scope). |
| 2 | **Starting a post**: reply `/status` to a message in a connector's Status topic, or `/status <text>`; or `/wuphf` in the Status topic, whose page has "📢 Post a status" (type the text). |
| 3 | **Content**: text, photo, video, voice/audio, a link. |
| 4 | **Confirmation**: a preview with ✅ Post / ✖️ Cancel, naming the WhatsApp account and the audience the phone's status privacy gives; previews expire. `/status!` skips it. |
| 5 | **Only owner ids** may post. Anyone else's `/status` is an ordinary (refused) message. |
| 6 | **Edits** of a posted story are refused (👎 + note, 083): WhatsApp can't edit stories. **Deletes** follow 083's `mirror_deletes_kept`. |
| 7 | **Mutes**: `stories.mirror` (all stories on/off, default on) and `stories.muted` (people whose stories are skipped), runtime settings. Muted stories are dropped entirely. |
| 8 | **Expiry**: `stories.expire_copies`, default off (keep forever); on, the Telegram copies of stories are deleted 24 h after they were posted. |

Planner decisions (2026-11-26; change them in review if wrong):

| # | Decision |
|---|---|
| 9 | **One command, two names**: `/status` and `/post` (085's channels) both mean "post this topic's source chat on purpose"; `!` right after the word skips the preview. The core recognizes them (like 081's lone emoji): only the owner's, only in a mirror topic, only for a source that takes posts. |
| 10 | **What is posted**: the message the command replies to (its text and attachments; that message is the story's logical message, so deleting it later deletes the story, decision 6). `/status <text>` without a reply posts the text, and the command message is the story's. Both: the replied-to message with `<text>` as its caption. |
| 11 | **Posts are a posting fact**: `ChatPosting.posts` (the account may post here on purpose) and `ChatPosting.audience` (who sees a post, as the platform's setting says: "My contacts", "My contacts except 3", "Only 5 contacts"). WhatsApp reports both for Status in 083's snapshot (`GetStatusPrivacy`), so the audience is as of the last connect. |
| 12 | **The preview is a panel page**: on `/status`, the core keeps a pending post (in memory, 10 minutes; a restart forgets it) and tells the Telegram connector to open the panel in that topic for the owner (`OpenPanelDispatch`); the panel opens on the pending post (botconf's `DataSource::open` by context). The core's single data entry becomes a small menu (🩺 Health, 📢 Posts). |
| 13 | **A link** is posted as a text status; WhatsApp shows the link, but no preview card (no fetching from the connectors: deferred). |
| 14 | **Text statuses** use WhatsApp's default look (a dark background, the default font). |

## Design

### On the wire

- `ChatPosting.posts` (bool), `ChatPosting.audience` (optional string).
- `MessageCreateDispatch.post` (bool): the explicit post. The only send the
  WhatsApp senders let through to `status@broadcast`.
- `OpenPanelDispatch { dispatch_id, target_chat_id, target_thread_id, actor }`:
  open the settings panel in that chat/topic for `actor` (an owner id).
  Acknowledged with the target chat id. Connectors without a panel
  acknowledge it unchanged (SDK default).

### Core

- `posts.rs`: `post_command(text)` → `{ force, rest }` for `/status`,
  `/status!`, `/post`, `/post!` (with an optional `@bot` suffix and text).
  In `ingest_create`, after storing an owner's message from a mirror
  endpoint: when it is a post command, route nothing for it, and either post
  at once (`force`) or keep a pending post and dispatch `OpenPanel` to the
  origin. A source that takes no posts, or a command with nothing to post,
  gets 083's refusal note.
- Posting: `Action::Send { post: true }` of the content message into the
  source; 083's screen lets a post through when the source `posts`.
- `Core::pending_post(context)`, `confirm_post(id)`, `cancel_post(id)` for
  the panel; `Core::compose_post(context, text)` for the page's "Post a
  status" (the text becomes a new owner message in the topic's
  conversation, posted at once after the page's own confirm).
- Mutes: at ingest, a message from a source chat with `private_replies`
  (a story feed) is dropped (`IgnoreReason::Muted`) when `stories.mirror` is
  off or its author is in `stories.muted` (any address form).
- Expiry: `Core::expire_stories(now)`: Telegram copies of feed messages
  older than 24 h are deleted (mirror endpoints only; the message is marked
  deleted). The runtime calls it every 10 minutes when
  `stories.expire_copies` is on. Storage: `messages_before(conversation,
  cutoff)`.

### Runtime (panel pages)

`post_pages::PostPages`: `open(context)` → the pending post of that topic,
or the topic's compose page when its source takes posts. `post/<id>`:
preview (account, audience, text or media kinds, expiry) with ✅ Post (Run)
and ✖️ Cancel (Run). `compose/<conversation>`: "📢 Post a status" (Ask text,
then posts). `data_pages::DataPages` routes `health/…` and `post…`, with a
home page linking both.

### Connectors

- **Telegram**: botconf go/v0.17.0 (the panel's `View.Context`, so the core
  knows the topic); `OpenPanelDispatch` → the panel command in that topic as
  that actor.
- **WhatsApp (Go and Rust)**: `posts` and `audience` for Status in the
  snapshot; a `post` send to `status@broadcast`: text (default look), photo,
  video, voice/audio, through the usual media path.

## Steps

1. Model/proto: `posts`, `audience`, `post`, `OpenPanel`; grpc; SDK default.
2. Core: post commands, pending posts, posting, the screen; tests.
3. WhatsApp: posting facts, status sends (both connectors); tests.
4. Runtime pages + Telegram (botconf go/v0.17.0, `OpenPanel`); tests.
5. Mutes and expiry (settings, ingest drop, storage query, runtime timer);
   tests.
6. Docs, notes, dev bins.

## Live test (owner)

1. Type a text in the Status topic (👎), reply `/status` to it → the panel
   opens on the preview → ✅ Post → the story is on your phone.
2. Reply `/status!` to a photo → posted at once.
3. `/wuphf` in the Status topic → 📢 Post a status → type → posted.
4. Mute someone in `stories.muted` → their next story does not arrive.
5. Turn on `stories.expire_copies` → a story copy older than 24 h goes.
