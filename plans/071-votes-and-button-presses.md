# Plan 071: Poll votes and button presses, both ways: the vote store, live tallies, and a Telegram press as the owner's action on WhatsApp

> **Executor instructions**: as plan 068. The vote store needs a migration
> with a `migration_tests` case.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED. It adds a new event pair and a migration; votes are bursty.
- **Depends on**: 068, 069, 070
- **Category**: core / storage / connectors

## Planner decisions (confirm with the owner)

1. **Who acts on WhatsApp is the bridge account, so only owners can act.** A
   Telegram press of a vote or business button is carried out as the WhatsApp
   account, which is the owner. So only the connector's `owner_ids` may
   press. Anyone else gets a toast ("Only the owner can vote or reply here")
   and nothing is sent. One account holds one set of votes per poll: the
   owner's latest press sets it.
2. **The core keeps the votes, one row per (message, voter)**, with the
   voter's chosen option ids. An empty set means withdrawn. The tally is
   computed from these rows, so a redelivered or duplicated vote is
   idempotent.
3. **Tallies coalesce.** For each target copy, a `PollTally` dispatch still
   pending is **replaced**, not queued behind. A burst of 30 votes edits the
   card a few times, not 30, and stays under Telegram's edit rate limits.

## Design

- `EventKind::PollVote { native_id, voter, voter_name, option_ids, from_me }`
  and `EventKind::ButtonPress { native_id, presser, button_id, from_me }`. In
  the proto, `PollVoteEvent` and `ButtonPressEvent` are `ClientFrame` cases.
- **WhatsApp → core.** whatsapp-rust decrypts a `pollUpdateMessage` with the
  poll's stored secret (`wacore::poll::decrypt_poll_vote_with_fallback`). The
  `selectedOptions` hashes are the option ids 069 assigned. It reports a
  `PollVote`. The voter's name comes the way reactions resolve theirs.
- **Core.** The core resolves `native_id` to the poll message, as for
  reactions; a vote for an unknown poll is ignored with a debug log. It
  upserts the vote and dispatches `Action::PollTally { message_id,
  native_id, tally: [{ option_id, voters: [{ id, name }] }] }` to every copy
  except the WhatsApp one (WhatsApp shows its own votes), with the coalescing
  in decision 3.
- **Telegram press → core.** A press on a tally card's option reports
  `PollVote` with `option_ids` = the owner's new set: a press toggles that
  option, and with `max_selections` 1 it replaces the set. A press on a
  business button reports `ButtonPress { button_id }`. Both go only from
  owners (decision 1).
- **Core → WhatsApp.**
  - A Telegram `PollVote` from an owner becomes `Action::CastVote {
    native_id, option_names }` to the poll's WhatsApp copy. whatsapp-rust
    votes as the account (`features/polls`, with the stored secret). The
    resulting echo, the account's own vote coming back, updates the same
    vote row, so nothing double-counts. The voter is recorded as the
    WhatsApp account, which is also how WhatsApp shows it.
  - `ButtonPress` becomes `Action::PressButton { native_id, button_id,
    button_text, kind }` to the business chat's copy. whatsapp-rust sends the
    matching reply:
    - a `buttonsResponseMessage` for a buttons message;
    - a `templateButtonReplyMessage` for a template;
    - a `listResponseMessage` for a list row;
    - an `interactiveResponseMessage` with the `nativeFlowResponseMessage` of
      a quick reply.

    Each is quoted to the original (its `contextInfo`), as WhatsApp's apps
    send them.
- **Telegram** applies `PollTally` by re-rendering its card (070); the
  owner's own vote is marked. It acknowledges a `PressButton` or `CastVote`
  it was never sent: it renders nothing.

## Steps

1. Migration `0020_poll_votes.sql` (`message_id`, `voter`, `voter_name`,
   `option_ids` JSON, `updated_ms`, primary key `(message_id, voter)`); a
   `PollVotes` role trait in `wuphf-storage` with memory and SQLite backends;
   parity tests; a `migration_tests` case.
2. The events and actions in the model and the proto, and the runtime
   conversions.
3. The core: vote ingest, tally dispatch with coalescing (a dispatch queue
   method that replaces a pending `PollTally` for the same target), press
   and vote routing to the origin copy, and owner checks. These are done by
   the connector, which knows `owner_ids`. The core only routes.
4. whatsapp-rust: vote decryption → `PollVote`; `CastVote`; the four reply
   kinds for `PressButton`. Tests with fixture secrets.
5. Telegram: the callback handlers (tally option, business button, call/copy
   toast), the owner check and its toast, and `PollTally` rendering.
6. Core tests:
   - a burst of votes yields one pending tally per target;
   - a withdrawn vote clears it;
   - a duplicated vote is idempotent;
   - the echo of the owner's vote does not double-count;
   - a press from a non-owner never reaches the core.
7. Gate, the dev binaries, and the owner's live test: vote on WhatsApp and
   watch the card; vote on Telegram and see the WhatsApp vote; press a
   business button and see the reply in WhatsApp.

## STOP conditions

- whatsapp-rust cannot vote with a secret recovered from storage (only from
  the live message). Report it; cast votes may then need the poll message
  re-fetched.
- WhatsApp rejects a `buttonsResponseMessage` or `templateButtonReplyMessage`
  from a companion device. Try `interactiveResponseMessage` instead, and
  report which kinds work.
