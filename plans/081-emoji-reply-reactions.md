# Plan 081: A lone-emoji reply from the owner is a reaction

> **Executor instructions**: as plan 080.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Depends on**: 080
- **Category**: core / Telegram connector

## Why

Telegram offers a fixed set of reaction emoji, and WhatsApp takes any. Since
080, the owner's native Telegram reactions reach WhatsApp, but only within
Telegram's set. Replying with a lone emoji lets the owner use any emoji.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Any reply whose whole text is one emoji** (no media, nothing else) becomes a reaction on the replied-to message, and is not sent as a message. To send a lone emoji as a message, add text or a second emoji. |
| 2 | **Only the owner** (`owner_ids`). Anyone else's emoji reply stays a message. |
| 3 | **The reply stays** in Telegram. It gets the bridge's 👍 once the reaction has been applied. Deleting the reply removes the reaction, and editing it to another lone emoji changes it. Editing it to anything else also removes it. |
| 4 | **The latest wins.** WhatsApp has one reaction per person: the latest native reaction or emoji reply is the owner's reaction there. Removing one does not bring back the other. |

## Design

- **Who sent it**: `MessageCreateEvent.sender_account`, the sender as
  `owner_ids` name accounts, when that differs from `sender`. Telegram sets
  it to a user's plain id, since `sender` is `telegram:user:N`. The core
  checks `is_owner(origin, sender_account or sender)`. The field is not
  stored on the message.
- **Detection** (core): a new message from the owner that replies to a
  stored message, has no attachments, and whose trimmed text is exactly one
  emoji (`emojis` crate; with or without U+FE0F). The reply is stored with
  its received copy, like any message, so a later edit or delete resolves to
  it, but it is not routed.
- **Storage**: no migration. The reaction is a row in 080's `reactions`
  table on the parent's copy at the origin endpoint, keyed
  `reply:<reply native id>`. A reaction-set diff ignores `reply:` rows, so a
  native reaction change never clears an emoji reply, and the reverse is
  also true.
- **Routing**: through 080's `reaction_changed`, with `by_owner`.
  `Action::React` gains `confirm: Option<ConfirmCopy { endpoint_id,
  native_id }>`, the reply to mark. When that reaction dispatch is
  acknowledged, the core sends a delivered receipt (👍) to the reply.
- **A clear only removes what it set** (decision 4): when an owner's clear
  is routed, a target whose stored `""` reaction is not the one being cleared
  (a later reaction from the other source replaced it) gets no dispatch.
  This holds for 080's native clears too.
- **Edit / delete** of a message that has a `reply:` row: a delete clears
  it; an edit to a lone emoji sets it, and an edit to anything else clears
  it. Neither is routed as an edit or a delete.

## Steps

1. Proto `sender_account`; model `NewMessage.author_account` and
   `Action::React.confirm`; grpc; Telegram sets it.
2. Core: detection, `reply:` rows, confirm on ack, edit/delete, the
   clear-only-what-it-set filter. Tests.
3. Docs (protocol, Components, Telegram README), notes, dev bins.
