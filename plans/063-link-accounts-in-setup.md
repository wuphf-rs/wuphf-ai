# Plan 063: Link a connector's account in its setup, and fetch its chats

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `CARGO_BUILD_JOBS=2 nice -n 19` / `GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19`;
> whatsapp-rust needs `RUSTC_BOOTSTRAP=1` and runs alone. Stage explicit
> paths only. A real login is the owner's to run: tests use the connectors'
> fakes.

## Status

- **Priority**: P1 — the owner asked for it (2026-11-23)
- **Effort**: L
- **Depends on**: botconf plan 017 (setup actions), plan 062 (the chat
  directory)
- **Category**: feature / DX

## Why this matters

A first setup today ends with starting each connector and watching its log
for a QR code or a login prompt. The owner wants `setup` to do it
(2026-11-23): link the account from the setup screen, keep the session where
the connector's config says (so the connector starts linked), and fetch the
account's chats for the pickers (plan 062).

## Design decisions (settled with the owner, 2026-11-23)

1. Each connector's setup gets two actions, on its saved config: **Link
   account** (link, then fetch the chats) and **Refresh chats** (fetch again
   on the linked session). Both refuse while the connector runs on that
   session (its session lock), and tell how to stop it.
2. **WhatsApp (Go and whatsapp-rust)**: `login_method` decides: a QR code
   drawn in the setup screen, refreshed as WhatsApp rotates it, or a pair
   code shown for the phone; after pairing, the chats are the joined groups
   and the contacts the session knows, waiting (with progress) for the first
   app-state sync, bounded. The connector's own relinking after a logout is
   unchanged.
3. **Telegram user session**: phone (from `WUPHF_TELEGRAM_PHONE`, else
   asked), the code, the 2FA password when set (masked); the chats are the
   dialogs. **Bot session**: Link account checks the token; a bot cannot list
   its chats, so there is no chat list (the action says so).
4. **Instagram**: the cookies (a file path, or pasted JSON), written to
   `cookies_file`; the session is checked, and the chats are the inbox
   threads.
5. Chats are written to the chat directory (plan 062) with the ids the
   connector reports to the core.

## Steps

1. Go: a shared helper for the actions' session lock and the directory
   write; WhatsApp (whatsmeow) link and chats; tests on fakes.
2. Telegram (gotd): the user flow through the setup's questions instead of
   the terminal prompt, the bot check, the dialogs; tests on fakes.
3. Instagram: the cookie import and check, the inbox threads; tests.
4. whatsapp-rust: the same as step 1 on whatsapp-rust; tests.
5. Docs (README: first setup links each connector from its setup), gate, dev
   binaries, row 063 DONE. Owner: link each account once from setup.
