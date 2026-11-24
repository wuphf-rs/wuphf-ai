# Plan 072: Telegram polls, locations, venues and contacts to WhatsApp, and the owner's Telegram vote as their WhatsApp vote

> **Executor instructions**: as plan 068.

## Status

- **Priority**: P2 (after 068–071)
- **Effort**: M
- **Depends on**: 068, 071
- **Category**: connectors (Telegram inbound, whatsapp-rust outbound)

## Design

- **Telegram inbound** (`updates.go`): set `content` from the message media:
  - `messageMediaPoll` → `Poll`: option ids are Telegram's option bytes,
    hex; `max_selections` is 1 unless `multiple_choice`. A quiz is a poll
    whose fallback text marks the right answer only after it closes; it is
    not revealed;
  - `messageMediaGeo` → `Location`;
  - `messageMediaGeoLive` → `Location { live }`, whose edits are content
    edits;
  - `messageMediaVenue` → `Location` with its name and address;
  - `messageMediaContact` → `Contacts`.
- **whatsapp-rust outbound** declares `content_kinds` = poll, location and
  contacts, and sends:
  - a poll with `features/polls` (create; keep the new poll's secret as 069
    does). WhatsApp allows 2–12 options; a Telegram poll with more is sent as
    the fallback text instead;
  - a `locationMessage`, or a `liveLocationMessage` for a live one, whose
    edits are sent as updates;
  - a `contactMessage` with one contact, or `contactsArrayMessage` with
    several.
- **Votes.**
  - **WhatsApp votes on a Telegram-origin poll:** they come in as
    `PollVote` (071) and the core sends `PollTally` to the Telegram copy.
    That copy is a native Telegram poll, which can't show WhatsApp votes, so
    the Telegram connector keeps a **tally reply**. It's a small card
    replying to the poll, made on the first WhatsApp vote and edited after.
  - **The owner's vote on a Telegram poll:** a user session sees its own
    choice (`messageMediaPoll.results` with `chosen`), and Telegram reports
    a change as `updateMessagePoll`. It reports a `PollVote` from the owner;
    the core sends `CastVote` to the WhatsApp copy. Other Telegram users'
    votes are not voted on WhatsApp: they are not WhatsApp users, and the
    native poll already shows them.

## Steps

1. Telegram inbound content, with tests from fixture updates.
2. whatsapp-rust outbound content and its capability declaration, with
   tests.
3. The tally reply on Telegram for a Telegram-origin poll; the owner's
   Telegram vote → `PollVote`.
4. Gate, the dev binaries, and the owner's live test in both directions.

## STOP conditions

- whatsapp-rust's poll creation does not return the new poll's message
  secret. Then a Telegram-origin poll's WhatsApp votes cannot be decrypted;
  report it before building the tally reply.
