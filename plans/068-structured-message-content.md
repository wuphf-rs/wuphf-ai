# Plan 068: Structured message content (polls, buttons, locations, contacts, events) in the model and the core

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> capped jobs, `nice`; never build on battery (`/sys/class/power_supply/AC/online`
> = 0 → stop and report). Read the whole plan series (068–073) before starting.

## Status

- **Priority**: P1. The owner asked for it on 2026-11-24. Today every WhatsApp
  poll, business buttons or list message, location, contact card and event is
  silently dropped (whatsapp-rust `normalize` returns `None`: no text, no
  attachment).
- **Effort**: L
- **Risk**: MED. It adds a field to the logical message and the dispatch
  contract, and both are additive.
- **Depends on**: none. It is the base of 069–073.
- **Category**: model / core / proto

## The series

| Plan | What |
|---|---|
| 068 | `Content` on the logical message, the proto contract, connector capabilities, the core's text fallback |
| 069 | whatsapp-rust inbound: polls, buttons, lists, templates, interactive, locations (live too), contacts, events |
| 070 | Telegram rendering: native location and contact, the poll tally card, pressable buttons through the helper bot |
| 071 | Votes and button presses: the vote store, live tallies, a press or vote on Telegram becomes the owner's on WhatsApp |
| 072 | Telegram → WhatsApp: Telegram polls, locations, venues and contacts as native WhatsApp messages, the owner's Telegram vote as their WhatsApp vote |
| 073 | The Go WhatsApp connector (whatsmeow): 069, 071 and 072's connector work, through whatsmeow's poll and message-secret helpers |

## Design decisions (settled with the owner, 2026-11-24)

1. **Types:** polls with votes; business buttons, lists, templates and
   interactive (native flow) messages with their replies; locations (pinned
   and live); contact cards; WhatsApp events.
2. **Polls on Telegram are a live tally card.** It is a message holding the
   question and, for each option, the WhatsApp votes (count and voters),
   edited in place as votes arrive. It is not a native Telegram poll, which
   could not show WhatsApp votes.
3. **Business buttons are pressable on Telegram.** Pressing one sends that
   button's reply to the business on WhatsApp.
4. **Both directions** (072).

## Planner decisions (within the settled scope; flag any doubt to the owner)

1. **Content is structured, in the core.** `Message` gains `content:
   Option<Content>`, an enum with these variants:
   - `Poll { question, options: [{ id, text }], max_selections }` (0 = any
     number);
   - `Location { latitude, longitude, name, address, url, live: Option<{
     accuracy_m, caption, sequence }> }`;
   - `Contacts { contacts: [{ display_name, vcard }] }`;
   - `Interactive { header, body, footer, buttons: [{ id, text, kind }], list:
     Option<{ button_text, sections: [{ title, rows: [{ id, title,
     description }] }] }> }`, where `kind` is `reply`, `url { url }`, `call {
     phone }` or `copy { code }`;
   - `Event { name, description, start_ms, end_ms, location, join_link,
     canceled }`.

   The message's `text` stays its caption or body, so an attachment, a caption
   and content can coexist (a template with a header image).
2. **Stored in the existing message JSON.** `#[serde(default,
   skip_serializing_if = "Option::is_none")]`, an additive field, so it needs
   no migration. The memory note on migrations covers renames, not additions.
3. **Connectors declare what they render.** `Capabilities` gains `repeated
   ContentKind content_kinds`. The core sends `content` only to a connector
   declaring that kind. To anyone else it sends a **text fallback** composed
   in one place, `wuphf-core/src/content_text.rs`, next to `notes.rs`. The
   fallback is appended to the message's text; attachments are kept. Instagram
   renders everything through the fallback unchanged; the Go WhatsApp
   connector does too until plan 073.
4. **Proto:** `MessageCreateEvent.content = 13` and `MessageCreateDispatch.content
   = 15`, a `Content` message with a `oneof` mirroring the model. An edit
   carries content too: `MessageEditEvent.content`,
   `MessageEditDispatch.content`. That covers live location updates and
   edited polls.
5. **Edits:** an edit may replace `content` as well as `text`. The core's
   no-op check compares both.

## Steps

### Step 0: Baseline

`cargo test -p wuphf-model -p wuphf-core -p wuphf-storage -p wuphf-runtime`
(capped jobs) is green. Note the counts.

### Step 1: The model

- `crates/wuphf-model/src/content.rs`: `Content` and its parts, with serde
  derives, and `Content::kind() -> ContentKind`. Export from `lib.rs`.
- `Message.content`, `NewMessage.content`, and `EventKind::Edit { content }`,
  all serde-default.
- Tests: a message stored before this change, with no `content` key, still
  deserializes; every variant round-trips.

### Step 2: The proto

- `common.proto`: `Content` (oneof `poll`, `location`, `contacts`,
  `interactive`, `event`), `ContentKind`, and `Capabilities.content_kinds`.
- `client.proto` / `server.proto`: the fields in decision 4. Regenerate the Go
  bindings with `scripts/gen-proto-go.sh` and commit them.
- `wuphf-runtime`: convert in both directions, and record the declared kinds
  per connector session (beside the other capabilities).

### Step 3: The text fallback

- `wuphf-core/src/content_text.rs`: one function per kind, for example:
  - a poll: `📊 Poll: <question>` then `1. <option>` per line, and "(choose up
    to N)" when it has a limit;
  - a location: `📍 <name>, <address>`, then the coordinates and a
    `https://maps.google.com/?q=lat,lon` link; live ones say "Live
    location";
  - a contact: `👤 <name>`, then the vCard's phone numbers;
  - interactive: the header, body and footer, then each button as `[<text>]`
    (a URL button with its link), and each list section with its rows;
  - an event: `📅 <name>`, then the time span (UTC and the owner's
    zone if configured, else UTC), the place and the link, with "Canceled" if
    so.
- Golden tests per kind.

### Step 4: Routing

- `send.rs` (or wherever `Action::Send` is built): for each target, if its
  connector declares `content.kind()`, keep `content`; else set `content: None`
  and append the fallback to `text`. The same goes for edits.
- Tests: a capable target gets the content; an incapable one gets the text;
  an edit that changes only the content is not suppressed as a no-op.

### Step 5: Docs

`docs/architecture.md` (Replicated Operations: content and its fallback),
`docs/connector-protocol.md` (the new fields and capability).

## Verification

- The new and existing tests pass.
- A stored message from before the change still loads.
- `cargo clippy` is clean for the touched crates.

## STOP conditions

- Messages are not stored as one JSON payload (`messages.payload`) any more,
  so adding a field would need a migration. Stop, and plan the migration first.
- `Capabilities` cannot carry a repeated field without breaking older
  connectors (it should not: proto3 is additive).
