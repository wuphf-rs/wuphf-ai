# Plan 077: Merge two address forms of one chat, and split a wrong merge

> **Executor instructions**: as plan 058. This is the riskiest operation of
> the data series: read `acknowledge.rs::migrate_identity` and the address
> links (`address_links`, `aliases`) fully before starting.

## Status

- **Priority**: P2
- **Effort**: M–L
- **Risk**: HIGH. It touches identity, which the core alone owns (AGENTS.md).
  The core's own LID ↔ phone migration must keep working and must not undo a
  manual merge or split.
- **Depends on**: 074, 075
- **Category**: core / storage

## Design decisions (settled with the owner, 2026-11-24)

1. The panel shows a chat's address forms (LID, phone number, …).
2. **Merge** two forms of one chat. When each already has its own mirror
   topic, the owner chooses which topic survives. The other is renamed
   "<title> (merged)" and closed (`Action::CloseThread`, 074), and its copies
   stay mapped so old replies still resolve.
3. **Split** a wrong merge: **nothing moves**. History stays with the
   original conversation; only the split-off form's future messages go to a
   new conversation, and a fresh topic.
4. Both are confirmed, permanent, audited and announced (074's rules).

## Design

- **Merge** reuses the core's identity migration (`migrate_identity`): it
  records the second form as an alias of the first, and folds the second
  conversation's endpoints, messages and copies into the surviving one. It
  differs in that the owner picks the surviving topic, and it is recorded as
  `manual` in the link, so the automatic migration never reverses it.
- **Split** removes the alias, and records a `manual_split` that the
  automatic migration honours. Without it, the next LID/phone mapping from
  WhatsApp would merge the two again. The split-off form provisions a fresh
  conversation on its next message.
- **Panel:** the conversation page lists its forms; Merge picks the other
  conversation (search) and then the surviving topic (`Choose`); Split picks
  the form to split off. Each ends in a `Confirm`.

## Steps

1. Map every place identity lives (`aliases`, `address_links`, endpoints,
   `conversation_endpoints`, chat profiles keyed by form) and write it down
   here.
2. `DataOps::merge` and `DataOps::split` with the `manual` flags, so the
   automatic migration honours them. Core tests:
   - a merge keeps every old reply resolvable;
   - the losing topic is renamed and closed;
   - a split routes the next message of the split-off form to a fresh
     conversation;
   - WhatsApp's next LID mapping does not re-merge a split, or undo a merge.
3. The panel pages (075) and CLI (`wuphf-server data conversation <id>
   merge --with <id> --keep-topic <id> | split --form <address>`).
4. Docs: architecture (Address Identity: manual merges and splits).

## STOP conditions

- Identity is keyed in a way that can't record a manual split without a
  schema change. Then plan the migration first and ask the owner.
