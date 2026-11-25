# Plan 082: Chat pickers search and page through every chat; chat names say their connector

> **Executor instructions**: as plan 080.

## Status

- **Priority**: P1
- **Effort**: S–M
- **Depends on**: botconf `v0.18.0` (its plan 019)
- **Category**: settings panel / core

## Why

Reported by the owner (2026-11-25):

- The chat picker for wuphf-misc's Instagram chats in `/wuphf` did not show
  all the chats. The panel offered only the first 8, and a typed name was
  taken only when exactly one chat matched it.
- The same WhatsApp group is on both of the owner's WhatsApp connectors, and
  nothing told the two apart.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Every chat picker searches all the chats.** The fix is in botconf (plan 019), so every reference setting gets it: rule chats, `notify`, chat-state overrides, plugin settings. |
| 2 | **Chat names in the panel say their connector**: `Family · whatsapp-main`, on buttons and in lists. |
| 3 | **On the Telegram mirror, only the info card says it** (`🔌 whatsapp-main`). Topic titles stay as they are. |

## Design

- **botconf `v0.18.0`.**
  - Reference questions, single and list, show pages of 8 candidates with
    ◀ / ▶. Chosen ones are marked ✅ / 🔘.
  - A typed name that matches several chats asks again with those
    matches.
  - `Names::candidates_page`.
- **`WuphfNames`** (`settings_auth.rs`).
  - `candidates_page` pages through the observed chats (most recent
    first) and through connectors and rules. `candidates` is its first
    page.
  - A titled chat's name is `<title> · <connector>`. Thread names already
    are `<chat> › <thread>`, now with the connector after them.
  - On buttons, a title over 28 characters is cut (`…`), so the connector
    still shows.
  - An untitled chat shows its id, which names its connector already.
- **Info card** (`notes::info_card`): a `🔌 <connector>` line after the title,
  naming the source chat's connector (`conversation.source`).
- **Health**: "has no topics" was the one chat check whose text did not name
  the connector. It now reads `<chat> on <connector> has no topics`.

## Steps

1. botconf plan 019, tag `v0.18.0`.
2. Bump botconf in `Cargo.toml` and `connectors/whatsapp-rust/Cargo.toml`,
   and both locks.
3. `WuphfNames`: labels and `candidates_page`; tests.
4. The info card's connector line; the health text; tests.
5. Gate: workspace tests, clippy, fmt. Rebuild the dev binaries.

## Live test (the owner)

1. In `/wuphf` → misc settings → Instagram downloader chats → ➕ Add:
   - the first 8 chats show with ▶, and ▶ / ◀ page through all of them;
   - typing part of a name lists the chats that match;
   - every button names its connector.
2. A rule's source chat (✏️ Change) offers the chats the same way, with 🔘 on
   the current one.
3. A chat that has not had a topic yet: its new topic's info card has the
   `🔌` line.
