# Plan 069: whatsapp-rust inbound polls, buttons, lists, templates, interactive messages, locations, contacts and events

> **Executor instructions**: as plan 068. The whatsapp-rust workspace builds
> alone, with `RUSTC_BOOTSTRAP=1` and `CARGO_BUILD_JOBS=1`.

## Status

- **Priority**: P1
- **Effort**: M–L
- **Depends on**: 068
- **Category**: connector (whatsapp-rust)

## Why this matters

These messages never reach the core today (`normalize.rs`: no text and no
attachment means `None`).

## Design (plan 068's settled scope)

`normalize` reads the base message (wrappers already looked through) and
sets `MessageCreateEvent.content`:

| WhatsApp message | Content |
|---|---|
| `pollCreationMessage` (V1–V3) | `Poll`: options keyed by their hash. A vote names an option by the SHA-256 of its text, so the id *is* that hash, hex. `max_selections` = `selectableOptionsCount` |
| `locationMessage` | `Location`: degrees, name, address, url |
| `liveLocationMessage` | `Location { live }`. Later updates of the same live share are edits (`MessageEditEvent.content`) of the first, matched by the message key whatsapp-rust reports |
| `contactMessage` / `contactsArrayMessage` | `Contacts`: display names and vCards |
| `buttonsMessage` | `Interactive`: content text, footer, header (text, or an image or document fetched as an attachment as today), reply buttons |
| `templateMessage` (hydrated four-row template) | `Interactive`: title, body, footer, quick reply / url / call buttons |
| `listMessage` | `Interactive { list }`: description as the body, `buttonText`, sections and rows |
| `interactiveMessage` (native flow) | `Interactive`: body, footer, header; `nativeFlowMessage.buttons` by name: `quick_reply` (reply), `cta_url` (url), `cta_call` (call), `cta_copy` (copy), from each button's `buttonParamsJson`. Unknown flows become a reply button labelled with their display text, or are dropped if they have none |
| `eventMessage` | `Event`: name, description, start/end (seconds → ms), location, join link, canceled |

- The **poll's message secret** (`messageContextInfo.messageSecret`) is kept by
  the connector in its own store, keyed by poll id: 071 needs it to decrypt
  votes and to vote. Read it now, while the message is in hand. It stays in
  the connector's table (a migration in its own SQLite), never in the core,
  because it is a platform secret.
- **The replies a business receives** (`buttonsResponseMessage`,
  `listResponseMessage`, `templateButtonReplyMessage`,
  `interactiveResponseMessage`) are ordinary messages from the user's side:
  bridge them as text, the selected button's text or title, as a reply to the
  original when the context names it.
- `Capabilities.content_kinds` stays **empty** for now: whatsapp-rust renders
  nothing structured until 072. Until then the core sends it the fallback text.

## Steps

1. Baseline: the whatsapp-rust tests pass (note the count).
2. A `content.rs` module beside `normalize.rs` with one function per message
   kind returning `Option<pb::Content>`. `normalize` calls it and lets a
   message through when it has content, text or attachments.
3. Live location edits. Poll secrets, with a migration of the connector's own
   session database.
4. Response messages as text replies.
5. Tests: fixtures per kind, built as `wa::Message` values the way the
   existing `normalize` tests build them; a wrapped (ephemeral or view-once)
   interactive message; a template with an image header.
6. The dev binary rebuilt (see the stale-dev-bins memory), and the owner's
   live test with a real business chat and a poll.

## STOP conditions

- whatsapp-rust's `get_base_message` does not look through
  `viewOnceMessageV2`, `documentWithCaptionMessage` or `ephemeralMessage` for
  these kinds. Look first; extend it in the connector, not the library.
- The poll option hash in whatsapp-rust's `wacore::poll` is not SHA-256 of the
  option name. Then use whatever `decrypt_poll_vote` matches against, so
  votes (071) resolve to these ids.
