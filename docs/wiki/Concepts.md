# Concepts

The few ideas everything else builds on. [Architecture](../architecture.md)
has the full model.

## Messages and copies

Every message has one **logical message** in the core and a **copy** on each
platform it reached. When you answer a mirrored WhatsApp message in Telegram,
the core knows which WhatsApp message your reply quotes. An edit or delete
reaches every copy, and a copy the bridge itself sent is never bridged back.

## Connectors and the core

- A **connector** is a thin pipe to one account on one platform. It reports
  what happens ("a message arrived in chat X") and does what the core asks
  ("send this into chat Y, as a reply to message Z"). It never decides where
  anything goes.
- The **core** (`wuphf-server`) decides everything: which chats mirror which,
  identity, deduplication and reply mapping. It keeps all of it in one SQLite
  database.
- Connectors connect to the core over gRPC, each with its own **token**. The
  core accepts only the connectors its config lists.

## Chats are written `connector:chat`

Every chat in a config is one string: the connector id, a colon, then the
platform's chat id, and optionally `#thread`:

| Chat | Written |
|------|---------|
| A WhatsApp contact | `whatsapp-main:15551234567@s.whatsapp.net` |
| A WhatsApp group | `whatsapp-main:120363012345678901@g.us` |
| A Telegram user | `telegram-main:telegram:user:704338780` |
| A Telegram forum group | `telegram-main:telegram:channel:2060340733` |
| One topic in it | `telegram-main:telegram:channel:2060340733#12` |
| An Instagram thread | `instagram-main:340282366841710300949128` |

The setup screens suggest chats by name from the **chat directory** (filled
when a connector's account is linked), so you rarely type these by hand.

## Rules and conversations

**Rules** say which chats mirror each other. There are two kinds:

- `many-to-one`: every chat of a source connector (or only some, or all but
  some), **each in its own topic** of one destination forum group. This is the
  usual setup. The destination connector must support topics (Telegram).
- `one-to-one`: one chat mirrored into one other chat (or one topic).

A **conversation** is the set of chats a rule ties together: a WhatsApp chat
and its Telegram topic, say. Conversations and topics are created the first
time a chat has a message. Replication is two-way: what you write in the
topic goes to the WhatsApp chat.

A rule's id is how its conversations remember it. Renaming a rule starts new
topics; editing it in place keeps the existing ones.

## One chat, many addresses

WhatsApp reaches one contact by a phone number and by a hidden LID, and
switches between them. wuphf treats them as one chat. A rule may name a chat
by either form once the connector has seen both.

## The owner

You are the **owner**. Your own messages, from your phone or from a mirror
topic, are bridged like anyone else's. `owner_ids` in the core's config says
which platform accounts are you, and only they may change settings from a
chat (the `/wuphf` panel on Telegram).

## Notices

When something needs you, the core sends a **notice** to the chats in
`notify`:

- a connector lost its session, needs a new login, or is back;
- a WhatsApp connector needs to be linked again (with the QR code);
- media could not be downloaded;
- a new release is out.

A notice is never sent through the connector it is about: a logged-out
WhatsApp cannot tell you it is logged out. List a target on more than one
platform to stay reachable.

## Runtime settings

Most of the config is read once at startup. Some of it can change while
everything runs, from the Telegram panel or `wuphf-server settings`: `rules`,
`notify`, `chat_state`, and each connector's rendering, log level and sticker
options. A change is stored in the database as an **override** over the file,
and unsetting it goes back to the file's value. See
[Configuration → Runtime settings](Configuration.md#runtime-settings).

## Remote functions

Heavy or platform-specific work runs as **functions** served by separate
**providers**: `media.convert@1` (converting animated stickers, served by
`wuphf-mediaconv`) and `instagram.fetch_media@1` (served by the Instagram
connector, used by wuphf-misc). Providers connect to the core, which brokers
the calls. When no provider is running, the caller falls back: a sticker is
sent static or as a file.
