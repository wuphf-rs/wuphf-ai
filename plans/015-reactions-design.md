# Reactions design (as built)

Deliverable of the plan 015 spike. Written **after** the implementation landed
rather than before it: the spike's STOP condition "reaction handling already
partially implemented" fired on first audit, because reactions, delivery acks,
and read receipts shipped end to end in the session before this document. What
follows is therefore the design of record — every decision the spike asked for,
with the answer that is actually in the tree and a citation for it.

Where the shipped design differs from what the spike anticipated, the
difference is called out under **Deviation** with the reasoning.

## 1. Platform capability survey

| Platform | Receive | Send |
|---|---|---|
| WhatsApp | `waE2E.Message.ReactionMessage` — names the reacted message via `GetKey().GetID()`, carries the emoji as text, and an **empty** text clears the reactor's reaction. Read through `Normalizer.normalizeReaction` (`connectors/whatsapp/internal/whatsapp/updates.go:244`). Encrypted group reactions (`EncReactionMessage`) need client-side decryption and are **not** handled. | Not implemented — WhatsApp is a source, not a mirror (see §3). |
| WhatsApp receipts | `events.Receipt` with `types.ReceiptTypeDelivered` / `types.ReceiptTypeRead`, mapped by `receiptKind` and reported per message id by `Normalizer.HandleReceipt` (`updates.go:196`). Group receipts are dropped as per-participant noise. | n/a |
| Telegram | **Not implemented.** The connector emits no `ClientFrame_Reaction` or `ClientFrame_Receipt`; it is a mirror only. This is load-bearing — see the echo invariant in §3. | `tg.MessagesSendReactionRequest` + `tg.ReactionEmoji` (`connectors/telegram/internal/telegram/sender.go:208`), used **only** for the bridge's own signalling emoji. Mirrored reactions render as text instead (`sender.go:168`). |
| Instagram | `table.LSUpsertReaction` and `table.LSDeleteReaction` rows, carrying `MessageId`, `ActorId`, and `Reaction`; a delete is normalized as an empty emoji (`connectors/instagram/internal/instagram/updates.go:78-95`). The thread is recovered from the message→thread cache, so a reaction on a message the connector never observed is skipped with a warning. | Not implemented — Instagram is a source. |

## 2. Event vocabulary

**Adopted as the spike recommended**: one toggle event, not add/remove pairs.

```rust
EventPayload::ReactionSet {
    platform_message_id: String,
    reactor: String,
    reactor_name: Option<String>,
    emoji: Option<String>,   // None = the reactor cleared theirs
    from_me: bool,
}
```
(`crates/wuphf-model/src/message.rs:192`)

All three platforms are toggle-shaped — WhatsApp sends an empty reaction to
clear, Instagram sends a `LSDeleteReaction` row, Telegram replaces a user's
reaction set — so "this reactor's reaction on this message is now X, or
cleared" is the one vocabulary that needs no per-platform translation. Add/
remove pairs were rejected: they would force every connector to synthesize a
remove it never receives.

Receipts are a **separate** event, not a reaction:

```rust
EventPayload::ReceiptSet { platform_message_id, observer, from_me, kind: ReceiptKind }
```
(`message.rs:206`, `ReceiptKind` at `message.rs:218`)

**Deviation.** The spike scoped reactions alone. Delivery acks and read
receipts were folded into the same design because they compete for the same
display slot on the mirror side, and resolving that competition (§3) is the
central decision of the feature.

## 3. Identity, storage, and display

### No reaction table

**Decision: reactions are not stored.** The spike asked whether a
`message_reactions` table keyed by (message, user) was needed; it is not. A
reaction fans out through the same machinery as an edit — resolve the native
id to the logical message with `message_id_for_copy`, then fan to the other
copies (`crates/wuphf-core/src/ingest.rs:331`). The existing
`copy_at_endpoint` binding and blocked-dispatch release carry it
(`crates/wuphf-storage/src/shared.rs:44`), so a reaction that arrives before
its parent copy is delivered is held and bound on release, exactly like an
edit.

No stored reaction state is needed because **no reaction state is ever
compared**. That falls out of the display policy below.

### Display policy: mirrored reactions are text; only signals are native

The spike foresaw a hard problem: reactors are platform-native ids that do not
map across platforms, so every mirrored reaction would be applied by the bridge
account, and WhatsApp's one-reaction-per-user rule would let a message show at
most one. The spike offered last-writer-wins vs reaction-summary-as-text.

**Decision: neither — mirrored reactions render as a text note, and the native
reaction API is reserved for the bridge's own two signals.**

- A mirrored reaction posts an italic reply naming the reactor and emoji, or
  noting they cleared it (`sender.go:168`). Unlimited reactors per message, no
  identity mapping needed, and the reactor's real name survives.
- Delivery ack → native 👍; read receipt → native 👀 (`sender.go:197-203`).
  Read receipts are DM-only, since group receipts are per-participant noise.

This resolves the display conflict the spike could not have seen: acks and
mirrored reactions both want the reaction slot, and only one of them can have
it. Signals win the slot because they are exactly one emoji each and are
inherently per-message state; mirrored reactions have unbounded cardinality and
degrade gracefully to text.

### Echo safety

**No echo-suppression state is needed, and this is an invariant, not an
accident.** The bridge never applies a native reaction that any connector
reports back:

- Mirrored reactions are posted as ordinary text replies. They are not tracked
  as message copies (`ApplyReaction` returns the *parent's* native id unchanged,
  `sender.go:190`), so they cannot be mistaken for a copy or trigger a loop.
- The 👍/👀 the bridge applies natively land on Telegram, and the Telegram
  connector reports no inbound reactions at all.

⚠️ **If the Telegram connector ever gains inbound reaction reporting, this
invariant breaks**: the bridge's own 👍/👀 would be reported back and mirrored
to the source as "Me reacted 👍". That change must add suppression of the
bridge account's own reactions at the connector boundary.

Because nothing is compared, `from_me` on `ReactionSet` is deliberately unused
in the source→mirror direction; every reported reaction mirrors, including the
owner's own (`crates/wuphf-core/src/lib.rs:103-106`). `from_me` on `ReceiptSet`
*is* honoured — the owner reading their own message on another device is not a
peer acknowledgement (`ingest.rs:380`).

## 4. Protocol and dispatch

Inbound (`crates/wuphf-proto/client.proto`): `ReactionEvent` (:77),
`ReceiptEvent` (:104), `ReceiptKind` (:89). Decoded at
`crates/wuphf-runtime/src/grpc.rs:219,246`.

Outbound (`crates/wuphf-proto/server.proto`): `ReactionDispatch` (:82),
`DeliveryAckDispatch` (:96), `SeenReceiptDispatch` (:106). Encoded at
`grpc.rs:343,355,364`.

Model payloads: `DispatchPayload::Reaction` / `DeliveryAck` / `SeenReceipt`
(`crates/wuphf-model/src/dispatch.rs:46,56,63`), all carrying
`target_platform_message_id` so blocked release can bind it.

Routing: `route_reaction` (`crates/wuphf-router/src/routing.rs:130`) and
`route_receipt` (:159), both over the shared `fan_out_to_copies` helper.
Delivery acks originate on settlement instead of ingest —
`delivery_ack_for` fires when a mirror **create** is acknowledged
(`crates/wuphf-core/src/acknowledge.rs:64`), so an edit/delete/reaction ack
does not re-signal delivery.

**Deviation from the maintenance note.** The spike expected a
`supports_reactions` flag on `ConnectorCapabilities`. That was rejected: a
capability flag makes the *core* decide what to withhold, growing a matrix of
core-side branches per signal. Instead the core dispatches uniformly and the
connector opts in structurally, via optional Go interfaces `Reactor`,
`DeliveryAcker`, and `SeenReceiptMarker`
(`connectors/shared/coreclient/client.go:99-121`). A connector that implements
none acknowledges the dispatch unchanged through `ackOnly`, so the core stops
redelivering. `ConnectorCapabilities` stays limited to what actually gates
routing decisions (threads and topic creation).

This is why WhatsApp and Instagram need no reaction-send code at all: they are
sources, and the signals they receive settle as no-ops.

## 5. What was built, and what is left

Phases as executed: proto → model/core/router → Telegram (pilot, the only
mirror) → WhatsApp and Instagram sources. Storage needed no new table.

Test coverage in `crates/wuphf-core/src/tests.rs`: reaction after delivery
fans to the delivered copy; reaction before delivery blocks then releases with
the native id bound; reaction for an unknown message is ignored; read receipt
on a mirror copy surfaces a seen receipt on the origin; own receipt is ignored;
acknowledging a mirror create emits a delivery ack to the origin.

Open items, none blocking:

1. **Encrypted WhatsApp group reactions** (`EncReactionMessage`) are dropped —
   they need client-side decryption. Group reactions in unencrypted form work.
2. **No live verification.** The whole path is unit-tested but has never run
   against real traffic; the emoji-set limits Telegram enforces on
   `MessagesSendReaction` for the account type in use are unconfirmed.
3. **Instagram reactions depend on the message→thread cache**, which is
   bounded (plan 012) — a reaction on a message evicted from the cache is
   skipped with a warning rather than routed.
4. The Telegram echo invariant in §3 needs a suppression rule *before* anyone
   adds inbound reaction reporting to that connector.
