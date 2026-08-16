# Plan 015: Design spike — execute reactions end to end

> **Executor instructions**: This is a DESIGN SPIKE, not a build plan. The
> deliverable is a design document plus answered/open questions — code changes
> are limited to a throwaway branch prototype if needed and are NOT merged.
> Follow the steps; if a STOP condition occurs, stop and report. When done,
> update the status row in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-model/src/message.rs crates/wuphf-proto/ crates/wuphf-core/src/lib.rs`
> On mismatch with the excerpts below, re-verify before designing.

## Status

- **Priority**: P2 (direction)
- **Effort**: M for the spike; implementation estimated L (touches proto, core, router, storage, and all 3 connectors)
- **Risk**: LOW (spike is read/design-only)
- **Depends on**: none (but implementation should follow plans 007/008 landing)
- **Category**: direction
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

Reactions are the project's most explicitly promised missing feature, with
evidence at every layer:

- Model is ready: `Reaction { message_id, user_id, emoji }` and
  `EventPayload::ReactionAdd(Reaction)`
  (`crates/wuphf-model/src/message.rs:131-136,149`).
- Core explicitly rejects them: `EventPayload::ReactionAdd(_) =>
  Err(CoreError::UnsupportedEvent)` (`crates/wuphf-core/src/lib.rs:87`); the
  runtime already labels the event kind `"reaction"` for logging
  (`crates/wuphf-runtime/src/lib.rs:132`).
- Docs promise it twice: "Reactions are modelled but not yet executed"
  (`docs/architecture.md:78`) and the roadmap "Execute reactions"
  (`docs/architecture.md:110`).
- **The protocol cannot carry them yet**: `ClientFrame.body`
  (`crates/wuphf-proto/client.proto:72-80`) has no reaction event, and
  `ServerFrame.body` (`crates/wuphf-proto/server.proto:109-115`) has no
  reaction dispatch. Connectors cannot even report a reaction today — note
  `EventPayload::ReactionAdd` is dead code from the wire's perspective.

The spike's job is to answer the design questions cheaply BEFORE a large
implementation: a wrong identity model for reactions (e.g. ignoring removals)
would bake an echo loop or a dedup hole into the protocol.

## Current state facts the design must respect

- One logical message → many platform copies (`MessageCopy` maps internal id ↔
  per-endpoint native ids). A reaction references its parent by
  `message_id: MessageId` in the model, but a *connector* only knows the
  platform message id — the inbound frame must carry the native id and the
  core must resolve it via `message_id_for_copy`, exactly as edits do
  (`crates/wuphf-core/src/ingest.rs:218-230`).
- Echo prevention patterns to reuse: edits dedup by content comparison
  ("an edit whose text matches the stored text is dropped",
  `ingest.rs:214-217`); deletes by the `deleted` flag. Reactions need an
  analogous idempotence rule — a stored reaction set per (message, user,
  emoji), with re-adds dropped and our own mirrored reactions recognized when
  echoed back.
- The model has `ReactionAdd` but **no `ReactionRemove`** — platforms treat
  reactions as toggles (WhatsApp sends an empty reaction to clear; Telegram
  replaces the user's reaction set). An add-only design cannot represent
  clearing; the spike must decide the event vocabulary.
- Dispatch plumbing that fan-out will reuse: `DispatchPayload` variants and
  blocked-dispatch release (`crates/wuphf-model/src/dispatch.rs:9-51`;
  blocked binding in `crates/wuphf-storage/src/shared.rs:20-47` — note its
  `match` must gain a reaction arm).
- Platform capability reality check needed per connector: Telegram (gotd/td)
  bot/user reaction API constraints; WhatsApp (whatsmeow) reaction message
  type; Instagram (mautrix-meta lightspeed) reaction rows in `table.LSTable`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Locate reaction surface | `grep -rn -i reaction crates/ connectors/` | the sites listed above, plus any drift |
| Platform API survey | read vendored deps under `$(go env GOMODCACHE)` or upstream docs for gotd/whatsmeow/mautrix-meta | reaction send+receive entry points identified |

## Scope

**In scope**: a design document `plans/015-reactions-design.md` (the
deliverable), produced by the steps below. Throwaway prototype code on a
branch that is never merged.

**Out of scope**: any merged code change; proto changes; implementing even
"just the easy platform".

## Steps

### Step 1: Platform capability survey

For each connector library, record in the design doc: can it RECEIVE reaction
events (and does the event carry: native message id, sender, emoji,
add/remove)? Can it SEND reactions (and what are the limits — emoji set,
one-per-user, bot restrictions)? Cite the exact upstream type/function names.

### Step 2: Decide the event vocabulary

Recommend: replace `ReactionAdd(Reaction)` with a single
`ReactionSet { platform_message_id, user_id, emoji: Option<String> }`
normalized event ("this user's reaction on this message is now X / cleared"),
matching the toggle semantics of all three platforms and making redundant
reports naturally idempotent (compare against stored state, drop no-ops —
same pattern as edits). Document why add/remove pairs were rejected if you
concur, or argue otherwise.

### Step 3: Specify storage and identity

- New table `message_reactions(message_id, endpoint... )`? Decide: are
  reactions stored per logical message keyed by (user identity, target)? What
  is user identity across platforms (sender ids are platform-native strings —
  reactions do NOT map users across platforms; a reaction mirrors as "made by
  the bridge account" everywhere else. State this consequence explicitly:
  on the mirror side all reactions are rendered by the bridge user, and
  platforms with one-reaction-per-user semantics (WhatsApp) can show at most
  ONE mirrored reaction per message — the design must pick a policy:
  last-writer-wins vs reaction-summary-as-text).
- Dedup/echo: when our own mirrored reaction is reported back by the target
  platform, how is it recognized? (Candidate: record a "sent reaction copy"
  analogous to `record_sent_copy`.)

### Step 4: Specify protocol and dispatch changes

Draft (in the doc, not in code) the proto additions: `ReactionEvent` in
`ClientFrame`, `ReactionDispatch` in `ServerFrame`, and the
`DispatchPayload::Reaction*` variant including how blocked dispatch binding
behaves when the parent copy doesn't exist yet (reuse `waiting_for`).

### Step 5: Implementation plan sketch + open questions

End the doc with: ordered implementation phases (proto → storage → core →
one pilot connector → remaining connectors), the test list (mirror of plan
007's delete tests), and every unresolved question with a recommended answer.

## Done criteria

- [x] `plans/015-reactions-design.md` exists answering Steps 1-5
- [x] Each claim about a platform API cites a concrete upstream symbol
- [x] The one-reaction-per-user policy decision is stated with trade-offs
- [x] No merged code changes (`git status` clean apart from plans/)
- [x] `plans/README.md` status row updated

## Outcome

STOP condition 2 fired: reactions, delivery acks, and read receipts were
already implemented end to end when this spike was picked up, so the spike
could not run as a pre-implementation exercise. `015-reactions-design.md` was
written instead as the design **of record** — the same Steps 1-5, answered
with what is in the tree and cited to it, plus the decisions that differ from
what this plan anticipated:

- Delivery acks and read receipts were folded in, because they compete with
  mirrored reactions for the same display slot.
- Mirrored reactions render as **text**, not native reactions — neither of the
  two policies this plan offered (last-writer-wins / summary-as-text). The
  native reaction API is reserved for the bridge's 👍/👀 signals.
- No `message_reactions` table, and no echo-suppression state: the bridge never
  applies a native reaction that any connector reports back. The design doc
  records this as an invariant with the condition that would break it.
- No `supports_reactions` capability flag (this plan's maintenance note).
  Connectors opt in structurally through optional Go interfaces, with an
  ack-only fallback, so the core stays uniform.

## STOP conditions

- A platform library turns out unable to RECEIVE reactions at all (changes
  the feature's value proposition — report before designing around it).
- You find reaction handling already partially implemented beyond the sites
  listed (drift — re-audit first).

## Maintenance notes

- The implementation that follows this spike should land behind per-connector
  capability advertising (a `supports_reactions` capability), following the
  existing `ConnectorCapabilities` pattern
  (`crates/wuphf-model/src/connector.rs:11-14`).
