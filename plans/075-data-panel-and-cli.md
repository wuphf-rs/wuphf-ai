# Plan 075: The data panel in /wuphf (a botconf DataSource), /wuphf in a topic opens its conversation, and `wuphf-server data`

> **Executor instructions**: as plan 058. Never build on battery.

## Status

- **Priority**: P1
- **Effort**: L
- **Depends on**: botconf 018 (`v0.17.0`), 074
- **Category**: runtime / CLI / connector (Telegram)

## Design decisions (settled with the owner, 2026-11-24)

1. A "🗄 Data" entry on `/wuphf`'s first screen. The pages are botconf data
   pages, served by the Settings service with its authorizer: owners only.
2. **Getting around:**
   - `/wuphf` sent inside a mirror topic opens that conversation's page;
   - elsewhere, a paged list, latest activity first, with search by chat
     name;
   - text search within a conversation, newest first; a result shows the
     message's copies.
3. The same operations are on the CLI (`wuphf-server data …`), for when
   Telegram itself is broken.

## The pages

- **Data**: Conversations, Dispatches, Audit log, Media.
- **Conversations**: the paged list with search (an `Ask`). Each item is
  "<title> · <source connector> → <mirror topic>".
- **A conversation:**
  - its source chat, address forms, rule, mirror topic, paused or not, and
    message count;
  - actions: Messages, Search, Dispatches, Profile, Raw;
  - Unlink topic and Move to another topic, each with a `Confirm`;
  - Pause and Resume (076), and Merge and Split (077);
  - Delete history: a `Choose` between whole and older than N (an `Ask`),
    then a `Choose` of reach, then a `Confirm`;
  - Purge media (older than N);
  - Forget chat (`Confirm`).
- **Move to another topic:**
  - "Send /wuphf in the topic to move to". The pending move is kept per
    actor, for 10 minutes;
  - `/wuphf` sent in the target topic answers with a `Confirm`
    ("Move Family here?"). If another conversation owns the topic, it shows
    which one, with **Take over** (`Confirm`) and Cancel.
- **Messages and search results:** for each message, its time, sender and the
  start of its text. A message shows its copies (endpoint, native id) and
  Raw.
- **Dispatches:**
  - filters by pending or dead-lettered, by conversation, and by reason
    (grouped, with counts);
  - a dispatch shows its target, reason and message;
  - actions: Retry, Drop, Retry all and Drop all (per filter, `Confirm`);
  - Clear dead letters older than N.
- **Audit log:** newest first, paged.
- **Raw** (read-only, the tables allowed by 074): the row as pretty JSON in
  monospace, cut at the limit. A longer row is sent in full as a `.json`
  file, where the front end can.

## Telegram: /wuphf in a topic

`panel.go`'s `Take` already catches the command anywhere it is sent. It now
passes `Viewer.context` = the chat and topic it was sent in. For a user
session, the panel opens in the helper bot's private chat as today; the
command in the topic gets a short reply, "Opened in @<helper>", and is
deleted after a few seconds. The core's `DataSource::open` maps the topic to
its conversation (its mirror endpoint) and opens it. A topic that isn't a
mirror opens the Data root.

## CLI

`wuphf-server data` takes `--config` (like `settings`) and goes through the
running core with the observer token (`WUPHF_OBSERVER_TOKEN`), as
`settings reload` does:
- `conversations [--search TEXT]`;
- `conversation <id> [unlink | move --topic <chat#thread> [--take-over] |
  delete-history [--older-than DAYS] [--with-mirror-copies] | purge-media
  [--older-than DAYS] | forget]`;
- `dispatches [--dead] [--conversation ID] [--reason TEXT] [retry | drop]`;
- `audit`.

Destructive commands ask y/N unless `--yes`. The actor recorded is "cli".

## Steps

1. Pin botconf `v0.17.0`; implement `WuphfData: DataSource` in
   `wuphf-runtime` on top of `DataOps` (074), and register it.
2. The pages above, with page tests through botconf's testing helpers.
3. The pending move per actor; `open` from a context; Telegram's context
   passing and the "Opened in" reply.
4. `wuphf-server data` subcommands, with tests.
5. Docs: wiki Operations ("The data panel"), the CLI help.
6. Gate, dev binaries, and the owner's live test, including unlink, move
   with take over, and retrying today's `CHANNEL_FORUM_MISSING` dead letters.
