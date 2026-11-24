# Plan 070: Telegram renders locations, contacts, events, poll tally cards and pressable buttons

> **Executor instructions**: as plan 068. `GOMAXPROCS=2 GOFLAGS=-p=2 nice`.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED. A user session cannot attach buttons; they go through the
  helper bot's inline results (as labelled stickers do), and editing those
  later has to be proven first (Step 1).
- **Depends on**: 068 (069 to see it live)
- **Category**: connector (Telegram)

## Design

The Telegram connector declares `content_kinds` = location, contacts, event,
poll, interactive, and renders each one:

| Content | On Telegram |
|---|---|
| Location | A native geo point (`inputMediaGeoPoint`), or a venue (`inputMediaVenue`) when it has a name or address. A live one is a native live location (`inputMediaGeoLive`, period = the share's remaining time, else 8 h); its edits move the pin (`messages.editMessage` with a new geo) |
| Contacts | A native contact each (`inputMediaContact`: name, the first phone, the vCard) |
| Event | A text card: `📅 name`, time span, place, link, "Canceled" |
| Poll | The **tally card**: `📊 question`, then each option with its count and voters, e.g. `▸ Yes — 2 (Alice, Bob)`. A **vote button per option** sits under it (071 makes them work). Re-rendered on `PollTally` (071) |
| Interactive | Body (header in bold, footer in italics), and an inline keyboard: reply buttons and list rows as callback buttons, URL buttons as URL buttons, call/copy as callback buttons that answer with the number or code (a toast) |

The sender label and the reply-to work as for any message. Anything that
can't render natively falls back to the core's text (it is in `text` only for
connectors that do not declare the kind, so here render from `content`).

### Buttons for a user session

A **bot session** attaches the keyboard itself. A **user session** cannot;
it sends the card as the helper bot's **inline result** (`sticker_inline.go`
does this for labelled stickers), so the message carries the bot's keyboard.
Callbacks reach the helper bot. The connector maps each one, by a short token
in the callback data, to (message, button), the way `panel.go` maps its
buttons: a stored random key, like the panel's `TokenStore`.

- **The card must be editable later** (tallies, and an edited message). An
  inline-sent message is edited by the bot through its `inline_message_id`,
  which the bot learns from `chosen_inline_result`. That needs inline feedback
  turned on for the helper bot (`/setinlinefeedback` in @BotFather, to 100%).
  Step 1 proves this before anything else.
- **The helper's inline mode must be on** (`/setinline`). On 2026-11-24 the
  server's helper had it off, so stickers fell back to a label message
  (`BOT_INLINE_DISABLED`). When the helper connects, the connector checks both
  settings (`users.getFullUser` on itself: `bot_inline_placeholder`) and
  sends the owner a notice naming the missing ones, instead of failing at the
  first sticker or card.
- With no helper, or when it fails: a user session sends the card **without
  buttons** (text and tally only) and the connector logs once why.

## Steps

1. **Spike: editing an inline-sent message.** With the dev helper
   (@PropheciTestBot):
   - send an inline result with a keyboard as the user session;
   - receive `chosen_inline_result` (feedback on) and keep its
     `inline_message_id`;
   - edit the text and the keyboard through the bot
     (`messages.editInlineBotMessage`);
   - confirm that a press yields `updateInlineBotCallbackQuery`, with the id.

   Write down what works in this plan. STOP if a user session's inline
   message cannot be edited by the bot.
2. Location, venue, live location and contact sends, with tests against the
   fake client (`sender_test.go`'s style).
3. Event and interactive cards: text, the keyboard model, callback tokens
   (stored, 30-day TTL like the panel's), with tests.
4. The poll tally card: rendering (counts, voters, the owner's own vote
   marked), the keyboard, and in-place edits on `Action::Edit` of the
   content. `PollTally` comes in 071; here the card shows the empty tally.
5. Inline delivery for a user session; `chosen_inline_result` →
   `inline_message_id` recorded as the copy's edit handle. Then the helper
   settings check and its owner notice.
6. Gate (`go vet`, `go test ./...`), the dev binary, and the owner's live test
   on the iPhone and Desktop apps.

## STOP conditions

- Step 1 fails: the inline-sent card cannot be edited. Fall back to cards
  without buttons for a user session (a bot session keeps them), and ask the
  owner before going on.
- Telegram refuses a geo live period the source gave (it allows 60 s to
  24 h, or "until stopped"). Clamp the period, don't fail.
