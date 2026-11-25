# Plan 083: Receive-only chats, Status safety, replies that quote another chat

> **Executor instructions**: as plan 080 (one build at a time,
> `CARGO_BUILD_JOBS=2`, `nice`; whatsapp-rust with `CARGO_BUILD_JOBS=1` and
> the memory watchdog; never build on battery).

## Status

- **Priority**: P1
- **Effort**: L
- **Depends on**: — (081 for lone-emoji story reactions)
- **Category**: core / proto / settings / connectors (Telegram, both WhatsApp)

## Why

WhatsApp delivers everyone's stories as messages in one chat,
`status@broadcast`, and the many-to-one mirror gives it a Telegram topic like
any chat. The bridge is two-way and nothing special-cases that chat, so
anything typed in the Status topic (a message, a reply to a story) is
dispatched to `status@broadcast`. whatsmeow then posts it as a **status to
every contact** the status privacy allows (`getStatusBroadcastRecipients`).
The same risk exists, less dramatically, for channels you don't own and
admin-only groups, where a send can only fail.

Replies that quote a message in another topic or chat are also broken:
the core resolves a quote only at the reply's own endpoint, and the Telegram
connector ignores `reply_to_peer_id`, so a quote from another chat can match
an unrelated message with the same id.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Receive-only is a general per-chat property.** Connectors report it as a fact (Status; channels you don't own or admin; admin-only groups and community announcement groups where you're not admin); it clears by itself when the fact changes (you become admin). The owner can also mark any chat receive-only (`receive_only` runtime setting). Nothing un-marks a fact: Status can never be opened. |
| 2 | **What is blocked**: new messages and edits into a receive-only chat. Reactions pass where the platform allows them. Deletes follow decision 9. |
| 3 | **Feedback**: the refused Telegram message gets a 👎 reaction (Telegram's standard set has no 🚫) that stays, and a reply note saying why that deletes itself after 30 s. |
| 4 | **Stories stay in one Status topic.** Replies inside the Status topic are blocked like any message. |
| 5 | **Story reactions**: the owner's native reaction or lone-emoji reply (081) on a story reaches the poster as WhatsApp's own quick reaction does: an emoji message in the poster's DM quoting the story. Never a send to `status@broadcast` (whatsmeow cannot address only the poster). Clearing does nothing on WhatsApp. |
| 6 | **Quoting another chat** works within one connector: the quote is kept (WhatsApp `ContextInfo.remoteJid`), e.g. a group message quoted in a member's DM ("Reply privately"). Across connectors the quote is dropped and the message goes plain. |
| 7 | **Quoting a story** in the poster's own DM topic becomes a real status reply. Quoting it anywhere else is refused (👎 + note): it would show Bob's story to someone else. |
| 8 | **The Telegram connector reads `reply_to_peer_id`**, fixing the wrong-match bug. |
| 9 | **Per-connector "deleting the Telegram copy revokes it on the source"** (`mirror_deletes_kept`, a list of source connectors where it is *off*; empty = on everywhere = today's behaviour). Covers messages and stories alike. |
| 10 | **Backstop**: both WhatsApp senders refuse a new message or an edit to `status@broadcast` as permanent, whatever the core sends. Revokes (your own story) and reactions (decision 5) pass. Plan 084 adds the one explicit way to post. |

## Design

### Facts on the wire (connectors → core)

`ChatUpdate.posting` (new, optional):

```proto
message ChatPosting {
  bool messages = 1;        // this account may post new messages here
  bool reactions = 2;       // this account may react here
  bool private_replies = 3; // a message here may be quoted only in its author's direct chat
}
```

Stored on `ChatProfile.posting` (JSON column: no migration). Absent = unknown
= open. WhatsApp connectors report, as a snapshot when the session connects:

- `status@broadcast`: `{messages: false, reactions: true, private_replies: true}`;
- every joined group (one `GetJoinedGroups` / `groups().get_participating()`):
  announce-only and not admin → `{false, true, false}`, otherwise `{true, true, false}`;
- every followed channel: owner/admin → open, else `{false, true, false}`.

and live on a group's announce / admin change involving the account.

### Settings (core, runtime)

- `receive_only: Vec<ChatTarget>`: chats the owner marked (the source chat
  or its mirror topic, as `chat_state` overrides name chats).
- `mirror_deletes_kept: Vec<ConnectorId>`: source connectors whose chats keep
  a message when its mirror copy is deleted.

### Core

- **One filter, after routing** (`refusal.rs`): for a dispatch created from
  an event that arrived on a mirror endpoint, into the conversation's
  *source* endpoint:
  - `Send` / `Edit` into a receive-only source (fact or setting) → not
    enqueued; refusal;
  - `React` into a source whose `posting.reactions` is false → refusal;
  - `Delete` into a source on a connector in `mirror_deletes_kept` → dropped
    silently.
- **A refusal** is a new action `Refuse { message_id, native_id, reason,
  note_ttl_secs }` dispatched to the origin copy: the connector marks the copy
  (👎) and posts `reason` as a reply note that it deletes after
  `note_ttl_secs` (30). Records no copy. The reason is composed by the core
  (`"Not sent: <title> is receive-only."`).
- **Cross-chat quotes** (ingest): the quote resolves at the origin endpoint
  first; then, when the event names `reply_to_chat_id`, in that chat on the
  origin connector (any thread); otherwise in the origin's own chat on any
  thread (a Telegram forum's other topics). Storage gains
  `message_id_for_chat_copy(connector, chat_id, native_id)`.
- **Routing a reply whose parent is in another conversation** (`Router::route`):
  never blocks on the parent. It looks for the parent's copy on any endpoint
  of the target's connector; found → `ReplyContext` with `chat_id` (that
  copy's chat, when it differs from the target's); not found → sent plain.
  A parent whose source chat has `posting.private_replies` may be quoted only
  into its author's direct chat (the target chat's address forms include the
  parent's author); otherwise the send is refused ("Not sent: a story can
  only be quoted in its poster's chat.").

### Proto

- `MessageCreateEvent.reply_to_chat_id` (optional): the quoted message's
  chat, when it is not the message's own chat.
- `ReplyContext.chat_id` (optional): the quoted copy's chat at the target,
  when it is not the target chat.
- `ChatUpdate.posting` (above).
- `MessageRefusedDispatch { dispatch_id, target_chat_id, target_thread_id,
  target_platform_message_id, reason, note_ttl_secs }`, `ServerFrame.refused`.

### Connectors

- **Telegram**: `reply_to_peer_id` → `reply_to_chat_id`; render
  `MessageRefusedDispatch` (👎 reaction + reply note, deleted after the TTL;
  a restart before then leaves the note).
- **WhatsApp (Go and Rust)**: posting facts (above); `ReplyContext.chat_id` →
  `ContextInfo.remoteJid` with the quoted participant; a reaction to a
  `status@broadcast` copy → emoji message to the author's DM quoting the
  story; backstop refusing sends/edits to `status@broadcast`.

## Steps

1. **Backstop** in both WhatsApp senders; story reactions as DM emoji
   replies. Tests.
2. **Model + proto**: `ChatPosting`, `ChatProfile.posting`, `Refuse`,
   `ReplyContext.chat_id`, `reply_to_chat_id`; grpc conversions.
3. **Core**: settings `receive_only`, `mirror_deletes_kept`; the refusal
   filter; `Refuse` dispatches and acks. Tests.
4. **Core**: cross-chat quote resolution and routing, the story-quote rule.
   Storage `message_id_for_chat_copy` (both backends). Tests.
5. **Connectors**: Telegram `reply_to_chat_id` and refusal rendering;
   WhatsApp posting facts and remote quotes. Tests.
6. Docs (protocol, Components, READMEs), notes, dev bins.

## Live test (owner)

1. Type in the Status topic → 👎 + note (gone after 30 s); nothing on WhatsApp.
2. Reply to a story in the Status topic → same.
3. React ❤️ to Bob's story → Bob gets ❤️ in his DM quoting the story.
4. "Reply in another chat" from Bob's story into Bob's topic → Bob gets a
   status reply. Into Alice's topic → 👎 + note.
5. Quote a group message in a member's DM topic → WhatsApp shows the group
   quote ("Reply privately").
6. An admin-only group where you're not admin → typing there is refused.
7. Add `whatsapp-main` to `mirror_deletes_kept`, delete a Telegram copy of
   your own message → it stays on WhatsApp.

## Out of scope

- Posting stories and channel posts on purpose (084, 085).
- Inbound quotes of another chat (Bob replying to your story shows the story
  quoted in Telegram).
