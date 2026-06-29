# Plan 017: Design spike — per-side replication direction (one-way mirrors)

> **Executor instructions**: This is a DESIGN SPIKE, not a build plan. The
> deliverable is a design document; code changes are limited to a throwaway
> prototype that is never merged. Follow the steps; if a STOP condition
> occurs, stop and report. When done, update the status row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-model/src/routing.rs crates/wuphf-model/src/connector.rs crates/wuphf-router/src/`
> On mismatch with the excerpts below, re-verify before designing.

## Status

- **Priority**: P3 (direction — weakest demand evidence of the three spikes; validate before building)
- **Effort**: S for the spike; implementation estimated M
- **Risk**: LOW (spike only)
- **Depends on**: none (implementation would depend on plans/003 and 007)
- **Category**: direction
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

The routing model is fully symmetric: every endpoint in a replica set both
emits events and receives dispatches (`Router::route` fans to *all* other
endpoints, `crates/wuphf-router/src/routing.rs:33-37`; same for
`route_edit`/`route_delete`). The architecture's own roadmap heads the other
direction — "Symmetric N-to-N rules (every side both source and mirror)"
(`docs/architecture.md:109`) — but the *existing* deployment shape in
`config/example.toml` (WhatsApp/Instagram **sources** fanned into one Telegram
**mirror** supergroup) raises the inverse need: an operator may want the
mirror to be read-only — a message, edit, or delete typed into the Telegram
topic should NOT replicate back into the WhatsApp chat. Today nothing
expresses that: delivery policy (`DeliveryOptions`) shapes only *how* a target
renders an operation, never *whether* a side's events replicate at all.

This spike defines the semantics and config surface for per-side replication
direction, and checks it against the planned N-to-N evolution so the two
don't fight.

## Current state

- Symmetric fan-out: `route`/`route_edit`/`route_delete` iterate
  `replica_set.endpoints` minus origin
  (`crates/wuphf-router/src/routing.rs:33-37,111-115,167-171`); any endpoint
  can be an origin (ingest resolves whatever address matched a side,
  `crates/wuphf-core/src/lib.rs:57-67`).
- Mirror topics deliberately accept typed input today: aliases map a mirror
  thread back to the conversation ("Reverse lookup for a message typed
  directly into a mirror topic", `crates/wuphf-storage/src/lib.rs:89-95`), so
  read-only mirrors are a *policy choice*, not a current bug.
- Where direction could attach:
  - `RuleSide` (`crates/wuphf-model/src/routing.rs`, `pub struct RuleSide {
    connector, kind }`) — per-side, per-rule; serde `#[serde(flatten)]` of
    `SideKind`, so a new optional field is additive in TOML;
  - or `ConnectorCapabilities`/`DeliveryOptions`
    (`crates/wuphf-model/src/connector.rs:11-14,49-61`) — per-connector,
    wrong granularity for "this one rule's mirror is read-only" (the same
    Telegram connector may serve writable sides elsewhere). Note this
    trade-off in the doc.
- Enforcement points to evaluate: drop at ingest (cheapest; before dedup
  claim? after? must not claim_event and then drop, or a later re-enable
  would treat old messages as seen — actually claiming IS correct since the
  event was observed; reason this through in the doc) vs filter at routing
  (allows per-target asymmetry like A→B but not A→C). Cite
  `crates/wuphf-core/src/ingest.rs` flow when choosing.
- Config validation home if implemented: `RoutingConfig::validated`
  (plan 003).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Find every origin-resolution path | `grep -n 'resolve_origin\|match_source' crates/wuphf-core/src crates/wuphf-router/src -r` | the sites a direction check must cover |
| Confirm mirror-input wiring | `grep -n 'conversation_for_thread' crates/ -r` | reverse-lookup call sites |

## Scope

**In scope**: design document `plans/017-asymmetric-replication-design.md`.
Throwaway prototype allowed on a branch, never merged.

**Out of scope**: merged code; changing example config defaults; reaction
semantics (plan 015 owns those — but the doc must note that any direction
flag must apply to reactions when they land).

## Steps

### Step 1: Validate the demand honestly

Search the repo for evidence the operator wants this (config comments, TODOs,
git history mentioning one-way/read-only). The audit found the *shape*
(source→mirror deployment) but no explicit ask. If no evidence beyond shape,
say so in the doc and recommend deferring implementation until a real need —
"not worth building yet" is an acceptable spike outcome and still a complete
deliverable.

### Step 2: Define semantics

For the recommended design (suggested: optional `replicates: bool` — or an
enum `direction: in|out|both`, default `both` — on `RuleSide`):

- What happens to a message typed into a `direction = "out"`-less side?
  (Recommend: ingest resolves the endpoint, claims dedup, returns a new
  `IgnoreReason::SideNotReplicating` — visible in logs, no dispatches.)
- Do edits/deletes/(reactions) from that side follow the same gate? (Yes —
  one gate at origin-resolution, not per-payload checks.)
- Does the side still RECEIVE dispatches? (Yes — that is what "read-only
  mirror" means; receiving is governed by fan-out membership, unchanged.)
- Interaction with `Source` include/exclude filters and with the planned
  symmetric N-to-N rules (`docs/architecture.md:109`) — show one config
  example of each shape.

### Step 3: Specify enforcement and tests

Pick the enforcement point with rationale (ingest-gate vs router-filter; the
per-target-pair matrix variant is almost certainly over-engineering — argue
it away explicitly). List the validation rules for
`RoutingConfig::validated` (e.g. "a rule where NO side replicates outbound is
dead and rejected") and the test list (mirror plan 007's structure).

## Done criteria

- [ ] `plans/017-asymmetric-replication-design.md` exists with: demand evidence (or explicit absence), chosen semantics for all four questions in Step 2, enforcement decision, config examples, validation + test lists
- [ ] An explicit go / defer recommendation
- [ ] No merged code (`git status` clean apart from plans/)
- [ ] `plans/README.md` status row updated

## STOP conditions

- Symmetric N-to-N rules work has already started (search for
  `SideKind` changes since `15aa85f`) — the two designs must be produced
  together; report first.

## Maintenance notes

- If the verdict is "defer", record the trigger condition in the doc (e.g.
  "first time a mirror-typed message causes an unwanted echo into a source
  chat") so the decision is revisited on evidence, not memory.
