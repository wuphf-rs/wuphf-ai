# Plan 080: The owner's Telegram reactions as their own reaction on WhatsApp and Instagram

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `CARGO_BUILD_JOBS=2`, `nice`; whatsapp-rust with `CARGO_BUILD_JOBS=1` and
> the memory watchdog; never build on battery
> (`/sys/class/power_supply/AC/online` = 0). No local `nix build` of packages:
> the pre-commit hook checks the vendor hashes, and CI builds.

## Status

- **Priority**: P2
- **Effort**: M–L
- **Depends on**: — (independent of 079's remaining Health page)
- **Category**: core / storage / connectors (Telegram, both WhatsApp, Instagram)

## Why

Replies, edits and deletes typed in the Telegram mirror reach WhatsApp, but a
reaction does not. The Telegram connector ignores reaction updates, and
neither WhatsApp connector can send a reaction: a `ReactionDispatch` is
acknowledged and dropped. The owner reads and answers chats from Telegram, so
the only way to react to a WhatsApp message today is to pick up the phone.

Instagram has the opposite problem. It applies every reaction it is sent as
the owner's own, so a WhatsApp contact's 😂 turns into the owner's Instagram
reaction.

## Design decisions (asked of the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | **Only the owner's reactions cross natively.** A Telegram reaction by one of the connector's `owner_ids` becomes the owner's native reaction on WhatsApp (and Instagram). Anyone else's reaction is not sent to WhatsApp or Instagram. |
| 2 | **Telegram → WhatsApp only.** Reactions arriving *in* Telegram keep today's rendering: an italic text reply, including "Me reacted ❤️" for the owner's own WhatsApp reaction. |
| 3 | **The bridge's Telegram session is a separate account** (or a bot), not the owner's. The bridge's own 👍/👀 signals are therefore never the owner's reactions, and no signal-echo bookkeeping is needed. The Telegram connector drops reactions by its own account at the boundary, as plan 015 §3 required. A setup that runs the session as the owner's own account gets no reactions from Telegram (documented). |
| 4 | **Instagram follows the same rule**: it applies the owner's reactions natively and drops anyone else's. |

Planner decisions (no owner input needed; change them in review if wrong):

| # | Decision |
|---|---|
| P1 | **The core decides who the owner is**, from the settings' per-connector `owner_ids` (already loaded as `derived.owners`), or the event's `from_me` on connectors whose session *is* the owner's account (WhatsApp, Instagram). Connectors only report reactors. Plan 071 currently puts its owner check in the connector; it should switch to this one when it is executed. |
| P2 | **Reactions are stored now**, which reverses plan 015's "no reaction table". The table holds the last reaction each reactor has on each copy. It does two jobs: Telegram user sessions report the whole reaction set of a message, so a clear can only be found by diffing; and a reaction equal to the stored one is a no-op, which makes echoes and reconnect replays harmless. That is loop prevention in the core, as AGENTS.md asks. |
| P3 | Telegram premium allows up to three reactions per user, and WhatsApp allows one. The connector reports each reactor's **newest** reaction. Removing it while older ones stay makes the next newest the reaction. |
| P4 | A custom emoji is reported as its `alt` emoji (from `messages.getCustomEmojiDocuments`, cached by document id). A paid ⭐ reaction is skipped. |

## Design

### Flow

```
owner reacts ❤️ on a mirrored copy in a topic
  → Telegram connector: ReactionsEvent (user session) or ReactionEvent (bot)
  → core: diff against stored reactions at that Telegram endpoint
          → reactor 704338780 now ❤️ (was none); 704338780 ∈ owner_ids → by_owner
          → store (message, telegram endpoint, 704338780) = ❤️
          → fan out to the other copies, each ReactionDispatch with by_owner
            and the target copy's author; store (message, whatsapp endpoint,
            "") = ❤️, the reaction the bridge expects WhatsApp to show as ours
  → WhatsApp connector: by_owner → BuildReaction(chat, author, id, "❤️")
  → (echo, if WhatsApp or Instagram ever reports it: from_me ❤️ at the
     whatsapp endpoint equals the stored "" row → ignored)
```

### Telegram in

- **User session**: `UpdateMessageReactions{peer, msg_id, top_msg_id,
  reactions}`. The connector reads `reactions.recent_reactions`
  (`MessagePeerReaction{peer_id, reaction, date, my}`), drops `my` (its own
  account: the 👍/👀 signals), keeps each reactor's newest by `date`, and
  reports a new **`ReactionsEvent`**: the complete set of other accounts'
  reactions on that message. When `can_see_list` is false, or `results` is
  non-empty while `recent_reactions` is empty, it fetches
  `messages.getMessageReactionsList(peer, id, limit 100)` instead. The topic
  comes from `top_msg_id`.
- **Bot session**: `UpdateBotMessageReaction{peer, msg_id, actor,
  new_reactions}` names one actor, which maps directly onto the existing
  toggle `ReactionEvent` (the newest of `new_reactions`, or a clear when it is
  empty). An actor equal to the bot is dropped. The update carries no topic,
  so the connector reads the message's topic (`channels.getMessages`, reply
  header `reply_to_top_id`, or the message id itself for a topic's first
  message) and caches it by `(channel, msg_id)`, bounded. Telegram sends these
  only to a bot that is an admin of the group: documented in the Telegram
  README.
- Reactor name: from the update's entities through the peer cache, as
  messages do today.

### On the wire

- `client.proto`: `ReactionsEvent reactions_set = 14` in `ClientFrame.body`:
  `{chat_id, optional thread_id, platform_message_id, repeated
  ReactorReaction reactions}` and `ReactorReaction{sender, optional
  sender_name, emoji}`. Its comment states the contract: the complete set of
  *other* accounts' reactions, so an absent reactor has none.
- `server.proto`, `ReactionDispatch`: `bool by_owner = 8`, and the target
  copy's author, as a reply's quote carries it: `string target_sender = 9`,
  `optional string target_participant = 10`, `bool target_authored_by_bridge
  = 11`.
- `ReactionEvent`'s comment changes: `from_me` now means "the session
  account", which the core counts as the owner.

### Core

- **Model**: `EventKind::ReactionSet { native_id, reactions:
  Vec<ReactorReaction> }` beside the toggle `EventKind::Reaction`.
  `DispatchPayload::Reaction` gains `by_owner: bool` and `target_author:
  QuotedAuthor {author_id, participant, authored_by_bridge}`. Extract the
  author part of `ReplyContext` into this type so replies and reactions share it.
- **Owners**: `RoutingConfig` gains `owners: HashMap<ConnectorId,
  HashSet<String>>`, built from the same `derived.owners` the settings auth
  uses. `by_owner(origin, reactor, from_me) = from_me ||
  owners[origin].contains(reactor)`.
- **Storage** (`ReactionStore`, part of `Storage`; both backends):
  `reactions_at(message, endpoint) -> Vec<StoredReaction{reactor, emoji}>` and
  `set_reaction(message, endpoint, reactor, Option<emoji>)` (None deletes the
  row). The reactor key is `""` for `from_me`. Migration
  `0023_reactions.sql`: `reactions(message_id, endpoint_id, reactor, emoji,
  updated_at_ms, PRIMARY KEY (message_id, endpoint_id, reactor))`, keyed like
  `message_copies` (no foreign keys, as in the rest of the schema). Add a
  `migration_tests` case.
- **Ingest**:
  - Toggle (`Reaction`): if the stored reaction equals the new one, ignore it
    (`IgnoreReason::DuplicateEvent`). Otherwise store it and route it.
  - Set (`ReactionSet`): diff the snapshot against the stored rows for that
    copy, excluding `""`. Each change (set, changed, cleared) goes through the
    toggle path.
  - Routing: `route_reaction` takes `by_owner`. For each target it fills
    `target_author` from the stored message and `is_sent_copy(target
    endpoint, native id)`, as `send.rs` does for a quote. A blocked target
    fills it when it is released: check that `bind` on release covers the new
    fields.
  - After saving a `by_owner` reaction dispatch, store `(message, target
    endpoint, "") = emoji`. A Telegram target renders text and never reports
    `from_me`, and snapshots exclude `""`, so the row is harmless there. A
    dead-lettered reaction dispatch deletes that row, so a real reaction from
    the phone is not swallowed later.
- Plan 015's design doc gets an "Amended by 080" section: reactions are now
  stored, `from_me` is honoured, and the echo invariant is kept by the
  connector-boundary filter in decision 3.

### WhatsApp out (both connectors)

- `ApplyReaction` / `apply_reaction`: when `!by_owner`, acknowledge without
  sending. Otherwise address the chat with `target`, resolve the author with the
  helper that `quotedAuthor` / `quoted_author` becomes (it takes the shared
  author fields instead of a `ReplyContext`), and send:
  - Go: `client.BuildReaction(chat, author, id, emoji)` + `SendMessage`
    (an empty emoji removes the reaction), plus the `linkedSender` passthrough.
  - Rust: a `wa::Message` with `reaction_message: ReactionMessage{ key:
    MessageKey{remote_jid, from_me, id, participant (groups only)}, text,
    sender_timestamp_ms }`, the key shaped as the library's
    `MessageContext::message_key`, through the sender's client seam.
- Return the target id unchanged, as every reaction does.

### Instagram out

`ApplyReaction`: when `!by_owner`, acknowledge without sending. The rest is
unchanged.

## Steps

1. **Proto + Go bindings**: the messages above; `scripts/gen-proto-go.sh`.
   Lands with step 2 if the Rust side does not compile apart. The pre-commit
   hook then recomputes every Go `vendorHash`.
2. **Model, storage, migration**: `QuotedAuthor` extracted, the new event kind
   and dispatch fields, `ReactionStore` in both backends, `0023_reactions.sql`,
   storage tests and a migration test. `grpc.rs` decodes `ReactionsEvent` and
   encodes the new dispatch fields.
3. **Core**: owners in `RoutingConfig`, the toggle and set ingest, routing with
   `by_owner` and the target author, the expected `""` row and its removal on
   dead letter. Tests in `crates/wuphf-core/src/tests.rs`:
   - an owner's Telegram reaction reaches WhatsApp `by_owner` with the
     received author, or with `authored_by_bridge` for a bridge-sent copy;
   - a non-owner's reaction is dispatched with `by_owner` false;
   - a set that drops a reactor emits a clear, and a set equal to the stored
     one emits nothing;
   - a repeated toggle is ignored;
   - the WhatsApp `from_me` echo of an applied reaction is ignored, and a
     different later reaction from the phone still routes;
   - a reaction before the copy is delivered binds the author on release;
   - a dead-lettered reaction clears the expected row.
4. **Telegram connector**: `OnMessageReactions` (user) and
   `OnBotMessageReaction` (bot), with the `my`/self filter, newest per
   reactor, custom-emoji alt (cached), the paid-reaction skip, the
   reactions-list fallback, and the topic lookup for bot updates. Unit tests on
   the normalizers with fake API seams for both fetches.
5. **WhatsApp Go**: `ApplyReaction` on `Sender` and `linkedSender`, and the
   shared author helper. Tests against the fake client:
   - group received message: `participant` set and not from me;
   - bridge-sent copy: from me;
   - a DM;
   - an empty emoji removes the reaction;
   - non-owner: nothing is sent.
6. **whatsapp-rust**: `apply_reaction`, the same cases against the recording
   client (`CARGO_BUILD_JOBS=1`, watchdog).
7. **Instagram**: the `by_owner` gate, with a test that nothing is sent for a
   non-owner.
8. **Docs**:
   - `docs/wiki/Components.md`: a "Reactions in" row (Telegram: yours) and
     "Reactions out" (WhatsApp ✓ yours, Instagram ✓ yours).
   - `docs/connector-protocol.md`: `ReactionsEvent`, `by_owner`, the target
     author.
   - `docs/architecture.md`, the reactions section.
   - The Telegram README: the bridge must be a separate account, and a bot must
     be an admin to see reactions.
   - The plan 015 amendment.
9. **Gate and handover**: clippy, fmt, `go vet`, the touched crates' and
   modules' tests. Rebuild the touched dev bins: the server, telegram,
   whatsapp, whatsapp-rust and instagram. Write the notes in
   `plans/README.md` and mark the plan DONE pending the live test.

## Live test (owner)

The Telegram API behaviour is unconfirmed (plan 015 open item 1 was about
the same API). The connector logs each raw reaction update at debug, so the
first test also answers it:

1. Restart the server, telegram and both WhatsApp connectors.
2. React ❤️ in a topic to a mirrored WhatsApp group message. WhatsApp shows
   your ❤️ on the original.
3. Change it to 😂: WhatsApp switches. Remove it: WhatsApp clears it.
4. Do the same in a DM topic.
5. If nothing reaches WhatsApp, run the Telegram connector at debug and look
   for `reaction update`. If there is none, Telegram does not push reactions
   to this account for that chat. Report this: polling is out of scope.

## Out of scope

- Native reactions *into* Telegram (decision 2).
- Telegram reactions by anyone other than the owner reaching WhatsApp, as text
  or otherwise (decision 1).
- Reactions on the owner's own Telegram messages. The bridge account is not
  their author, and Telegram may not tell it: whatever arrives is handled,
  and nothing is polled.
- The bridge's 👀 replacing a reaction on a Telegram message (Telegram's
  one-reaction limit). This exists today and is unchanged.
