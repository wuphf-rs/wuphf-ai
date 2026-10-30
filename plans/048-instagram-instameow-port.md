# Plan 048: Port the Instagram connector from messagix to instameow

Status: DONE (executed directly, 2026-10-31; verified live by the owner).

## Why

The connector was pinned to `go.mau.fi/mautrix-meta v0.2607.0` and used
messagix's Instagram lightspeed socket. Upstream v26.07 was the last release
to support Instagram on that path; v26.08 removed it. As of 2026-10-30
Instagram no longer answers lightspeed requests: HTTP login works, but every
socket request (`failed to send sync tasks`, sends) times out. Upstream
Instagram support now lives in `pkg/instameow`, and `pkg/igconnector` is the
reference bridge built on it.

## Protocol facts the port relies on

- **Thread ids.** A thread has three ids:
  - `ThreadKey` (int64, the FB id). The stable chat id, the same value the old
    lightspeed `ThreadKey` used. igconnector keys portals by it
    (`MakeFBPortalID`), which is what makes its upgrade from the old bridge
    seamless.
  - `ID`, the "IG chat id" / short id. Deltas carry it as
    `Delta.ThreadIGID`. Text sends, edits and reactions address threads by it.
  - `ThreadID`, the "long id". Unsend and mark-read use it.

  `FetchThreadID(fbid)` returns both string ids for a `ThreadKey`, and
  `GetThread(id)` returns the full `ThreadInfo` for a short id.
- **Message ids.** `Message.ID` is the canonical id. Edits, deletes,
  reactions, replies and send responses all use it.
- **Users** are identified by `InteropMessagingUserFBID`, the same FB ids the
  old `SenderId` used.
- **Transport.** Sends, edits, unsends and reactions are graphql HTTP calls,
  so there is no socket "can send" gate. Receiving goes through the DGW socket
  (`Client.Connect`, which blocks and reconnects internally). Events arrive as
  `slidetypes.ClientEvent`.
- **Delivery.** A delta whose handler returns an error is not acknowledged:
  the client keeps its seq id and reconnects, so the delta is redelivered.
- **Offline catch-up.** A client started with a saved seq id
  (`SetSeqID` + `ReloadIndex`) replays deltas missed since then. If the seq id
  is too old, the server sends `ResnapshotRequired`, and a fresh `LoadIndex`
  is needed (the gap is lost).

## Design (connector stays thin; ids stay compatible)

The connector still emits `chat_id = ThreadKey` and `platform_message_id =
Message.ID`, so existing routing rules and stored copy mappings keep working.
No core, proto or migration changes.

`internal/instagram` files:

- `threads.go`: `ThreadDirectory`. The connector-local translation between
  platform ids (short id ↔ `ThreadKey` → long id), plus thread titles and user
  names. It is primed from the mailbox and fills misses through a narrow
  `ThreadFetcher` interface (`GetThread`, `FetchThreadID`). This is platform-id
  plumbing, not bridge identity, so it belongs in the connector.
- `updates.go`: `Normalizer` over `*slidetypes.Delta`. Handles new
  messages, admin text (excluding reaction action logs), edits, deletes,
  reaction create/delete (a per-log-id cache resolves deletes that omit the
  sender), and thread rename and participant changes (which refresh titles).
- `media.go`: maps `MessageContent` variants (image, video, multimedia,
  audio, animated, sticker, music sticker, raven, XMA) to media refs, then
  downloads them and uploads to the core broker. XMA posts and reels resolve
  through `FetchMedia`.
- `sender.go`: text send (with reply target), edit, unsend and reactions
  (new: `coreclient.Reactor`), all through graphql. Outbound attachments
  remain unimplemented, as before.
- `state.go`: a small JSON state file holding the seq id, so restarts catch
  up on missed deltas. This replaces the old inbox-snapshot replay.
- `connector.go`: lifecycle. Loads cookies, then either resumes (saved seq
  id: `ReloadIndex` + `GetMailbox` to prime titles) or starts fresh
  (`LoadIndex`). It runs `Connect`, re-snapshots on `ResnapshotRequired`,
  sends owner notices on auth failures, and persists cookies and state.

Config: new optional `state_file` (default `instagram.state.json`).
`backfill_on_start = false` means "ignore the saved seq id and start live".

## Verification

- `go vet ./... && go test ./...` in `connectors/instagram`, with the build
  limits from plan 045.
- Unit tests: directory resolution (cache, fetch on miss), delta
  normalization per event kind, media ref extraction, sender request
  shaping, state round-trip.
- Live: start the connector and confirm `Successfully subscribed to iris
  socket`, then bridge a message both ways, plus an edit, an unsend and a
  reaction.

## Follow-ups (not in this plan)

- Outbound attachments (`mediadl.ReuploadFileToMeta` is Matrix-coupled; it
  needs a connector-side uploader).
- Read receipts and seen markers.
