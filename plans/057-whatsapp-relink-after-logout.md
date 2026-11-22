# Plan 057: WhatsApp connectors link again by themselves after a logout

Owner request (2026-11-23): when WhatsApp logs a connector out, the connector
starts linking again on its own, by QR code or by the pair code typed into the
app, as configured; the owner gets the QR image or the code through the other
connectors (Telegram), not only in the connector's terminal.

## Today

- **Go (`connectors/whatsapp`)**: `Connector.Run` pairs by QR once, before the
  core session starts, then runs. On `events.LoggedOut` whatsmeow deletes the
  device itself (`Store.Delete`); the connector sends the logged-out notice and
  stays up, unlinked, until restarted. No pair-code login.
- **Rust (`connectors/whatsapp-rust`)**: the core session and the bot start
  together; the bot pairs by QR, or by pair code when `phone_number` is set.
  On a logout the library stops reconnecting and keeps the dead device in the
  session database; `supervise` keeps the connector up for the notice (052).
  Linking again means moving the session database aside and restarting.
- **Notices** (`OwnerNotice { kind, detail }`) carry text only. The core turns
  one into an `OwnerMessage` per notice target, and `OwnerMessage` already
  carries `media_ids` (050), so the core can deliver a picture with no new
  delivery path.

## Decisions

1. **`login_method`** in both connectors' configs: `"qr"` (default) or
   `"pair_code"`. `pair_code` needs `phone_number` (country code and number,
   digits only); a config that has one without the other is refused at start.
   Rust keeps `phone_number` and gains `login_method`; until now a set
   `phone_number` meant pair code, so an existing config with `phone_number`
   and no `login_method` keeps pairing by code (`login_method` defaults to
   `pair_code` when `phone_number` is set, else `qr`). Go gains both keys.
2. **Only a logout links again.** `LoggedOut` (the phone unlinked the device,
   or WhatsApp rejected the session) starts a new link. `StreamReplaced`
   (another client took the session), a temporary ban and an outdated client
   do not: linking again would fight the other client or fail the same way.
   They keep today's behaviour (notice, stay up).
3. **The core session stays up throughout.** Each connector splits into a core
   session that lives as long as the process and a WhatsApp session that it
   replaces after a logout. Dispatches that arrive while no session is linked
   fail as transient (`WhatsApp is not linked`), so the core retries them once
   the new session is linked. Go therefore starts the core session before
   pairing (today it starts after).
4. **A fresh device for every new link.** Go: whatsmeow has already deleted the
   device; `GetFirstDevice` hands out a new one. Rust: the session database
   (and its `-wal` / `-shm`) is renamed to `<name>.logged-out-<unix seconds>.db`
   (kept, never deleted) and a new one is opened at the same path. The
   connector's own tables in it (`wuphf_contacts`) start empty; the new link's
   full app-state and history sync fill them again.
5. **A new notice kind, `NOTICE_KIND_LINK_REQUIRED = 7`**, not about health
   (like 5 and 6): "is not linked to WhatsApp; link it again". `detail` says
   how (scan the attached QR in WhatsApp → Linked devices → Link a device, or
   enter the code after "Link with phone number instead"), and for how long it
   is valid. `OwnerNotice` gains `repeated string media_ids = 3`: the core
   attaches them to every message it sends for the notice. The QR is a PNG
   uploaded to the core's media store by the connector.
6. **One notice per pairing round, with growing gaps.** WhatsApp rotates a
   QR every 20-60 s and ends a pairing round after about 160 s; the libraries
   then start a new round. Every code still goes to the terminal; the owner is
   told about the first code of a round only when the gap since the last told
   code has passed: 0, then 5 min, 15 min, 30 min, then every hour, reset by a
   successful link. A told QR is stale within a minute, so the notice says
   when the next one comes. (A "send me a fresh code" owner command is the
   panel-command plan's job.)
7. **The QR image**: Go renders with `rsc.io/qr` (already in `go.sum`,
   `Code.PNG()`); Rust renders the `qrcode` crate's modules into a 1-bit PNG
   written by a small encoder over `flate2` (already in the lock), so no image
   crate is added.

## Steps

1. Proto, model, runtime, core: `NOTICE_KIND_LINK_REQUIRED`, notice media ids
   through `InboundWork::Notice` into `notify_owners`, Go bindings
   regenerated. Tests: the kind maps, bypasses the health state, and the
   notice's dispatches carry the media.
2. SDKs: Rust `notice_with_media` / `send_notice_with_media`; Go
   `coreclient.NoticeWithMedia` / `SendNoticeWithMedia`.
3. whatsapp-rust: `login_method` config; `qr_png`; a `Linking` component
   (rate limit, upload, notice) wired to `on_qr_code` / `on_pair_code`; the
   WhatsApp-session loop with a swappable client in the sender and the
   database moved aside after a logout.
4. Go WhatsApp: `login_method` / `phone_number` config; the same split (core
   session first, a WhatsApp-session loop, a sender that delegates to the
   linked session); pairing by QR channel or `PairPhone`; the same linking
   notices.
5. Docs (`docs/connector-protocol.md`, `docs/architecture.md`, README,
   both config examples), dev binaries rebuilt.

## Owner live test

Restart the server, Telegram and both WhatsApp connectors. Unlink one
connector's device on the phone: the logged-out notice arrives, then a
link-required notice with the QR (or the code). Link it: a recovered notice
follows, messages flow again, and anything sent to WhatsApp meanwhile is
delivered. Repeat with the other `login_method`.
