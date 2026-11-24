# Plan 073: The Go WhatsApp connector (whatsmeow) gets structured content, votes and button presses, both ways

> **Executor instructions**: as plan 068. `GOMAXPROCS=2 GOFLAGS=-p=2 nice`.

## Status

- **Priority**: P1. The owner wants both WhatsApp connectors to have the
  series (2026-11-24).
- **Effort**: L
- **Depends on**: 068 and 071 (the core and the contract); it mirrors 069,
  071 and 072 for whatsapp-rust. Do it after them, so the core's behaviour is
  proven.
- **Category**: connector (WhatsApp, Go)

## Design

The same mapping as whatsapp-rust, through whatsmeow. The core and the
contract are shared, so only the connector changes.

- **Inbound** (`internal/whatsapp/updates.go`, `normalize`): a `content.go`
  beside it builds `wuphfpb.Content` from the `waE2E.Message`, with the table
  in plan 069:
  - polls (V1–V3); option ids are the SHA-256 of the option name, hex, as
    whatsmeow's `DecryptPollVote` reports `SelectedOptions`;
  - locations; live locations, whose updates are content edits;
  - contacts and contact arrays;
  - buttons, templates, lists and interactive / native flow messages;
  - events.

  A business's received replies are bridged as text replies, as in 069.
- **Secrets:** whatsmeow keeps message secrets in its own store
  (`msgsecret.go`), filled when a message carrying one arrives. So unlike
  whatsapp-rust, the connector needs no table of its own. Check that it saves
  the poll's secret for messages the account sends too (`BuildPollCreation`,
  then send).
- **Votes in:** a `pollUpdateMessage` goes through `cli.DecryptPollVote`. The
  `SelectedOptions` hashes are the option ids; it reports `PollVoteEvent`.
- **Actions out:**
  - `CastVote` → `cli.BuildPollVote(pollInfo, optionNames)` and send.
    `pollInfo` is the stored poll's `MessageInfo`: chat, sender and id, from
    the connector's message cache. If it isn't there, rebuild it from the
    copy's key, which the dispatch carries.
  - `PressButton` → the reply kinds of plan 071, built as `waE2E.Message`
    values, each quoting the original through `ContextInfo`.
- **Outbound content** (072's set): `BuildPollCreation` for a poll (2–12
  options, else the fallback text), `LocationMessage` or
  `LiveLocationMessage`, `ContactMessage` or `ContactsArrayMessage`. It
  declares `content_kinds` = poll, location and contacts, as whatsapp-rust
  does after 072.
- **Shared code:** anything both Go connectors need, such as converting
  `wuphfpb.Content` to and from small Go structs, goes in `connectors/shared`.
  Nothing platform-specific goes there.

## Steps

1. Baseline: `go test ./...` in `connectors/whatsapp` and `connectors/shared`.
2. Inbound content and response messages as text, with fixtures per kind (the
   `updates_test.go` style), including wrapped messages (ephemeral,
   view-once, document with caption).
3. Vote decryption → `PollVoteEvent`. Test it with a vote built by
   `EncryptPollVote` against a known secret.
4. `CastVote` and `PressButton`, tested against the connector's fake
   client.
5. Outbound content and the capability declaration.
6. Gate, the dev binary, and the owner's live test. The Go connector needs
   its own account (`whatsapp-main` in dev), or one of jio and airtel moved
   over from whatsapp-rust for the test.

## STOP conditions

- whatsmeow's store does not keep the secret of a poll the account itself
  created. Then keep it in the connector (as 069 does), and say so.
- whatsmeow refuses to send a response message kind from a companion device.
  Report which kinds work, as in 071.
