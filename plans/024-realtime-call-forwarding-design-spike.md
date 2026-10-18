# Plan 024: Design spike — real-time WhatsApp ↔ Telegram call forwarding

> **Executor instructions**: This is a DESIGN SPIKE, not a build plan. The
> deliverable is a design document (`plans/024-realtime-call-forwarding-design.md`)
> plus answered/open questions and a phased implementation roadmap. Code changes
> are limited to a throwaway proof-of-concept on a branch that is NOT merged.
> Follow the steps; if a STOP condition fires, stop and report. When done,
> update the status row in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat HEAD -- crates/wuphf-proto/ docs/connector-protocol.md connectors/whatsapp connectors/telegram`
> and confirm the vendored library is present:
> `test -d vendor/whatsapp-rust/src/voip && echo OK`.
> On mismatch with the excerpts below, re-verify before designing.

## Status

- **Priority**: P3 (direction / new capability, not a correctness gap)
- **Effort**: M for the spike; implementation estimated **XL** (new real-time
  subsystem, a second WhatsApp client, a C++ FFI dependency, and audio
  transcoding — larger than any single existing connector)
- **Risk**: LOW for the spike (read/design + throwaway PoC only); HIGH for the
  implementation it scopes (account-suspension risk on both platforms; a
  latency-sensitive path with no analogue in the current codebase)
- **Depends on**: none for the spike
- **Category**: direction
- **Planned at**: commit `f7f0c16`, 2026-07-20

## Why this matters

wuphf today replicates **discrete messages** (create/edit/delete/reaction/
receipt) through a durable, at-least-once dispatch pipeline. A live voice call
is a fundamentally different object: a low-latency bidirectional RTP audio
stream with no "redeliver on reconnect" semantics. The user wants a WhatsApp
call to ring through to Telegram (and/or vice versa) as a real call they can
actually answer and talk on — not merely a "📞 missed call" text marker.

Until now the WhatsApp side made this impossible: the Go connector uses
`whatsmeow`, which exposes only call **signaling** events, never the encrypted
media plane. That blocker is gone. The repo now vendors
[`oxidezap/whatsapp-rust`](https://github.com/oxidezap/whatsapp-rust) under
`vendor/whatsapp-rust`, a pure-Rust WhatsApp client with a **full VoIP media
implementation**: it owns signaling, RTP timing, SRTP/WARP, relay transport,
and codecs, and exposes both a PCM path and a raw-Opus "encoded" boundary
(`vendor/whatsapp-rust/agent_docs/voip_audio_codecs.md`;
`vendor/whatsapp-rust/wacore/src/voip/`). Call control is
`client.voip().call() / accept() / reject() / terminate()`
(`vendor/whatsapp-rust/src/client/voip.rs:85-123`). MIT-licensed, pure Rust —
it could live inside wuphf's Rust core rather than behind a Go connector.

The purpose of a spike (not a direct build) is that this feature has an unusual
concentration of **architecture-deciding unknowns** that a wrong first guess
would bake in expensively:

1. Real-time media does not fit the connector protocol or the dispatch
   pipeline at all — it needs a parallel subsystem, and *where* it lives
   (in-core Rust vs a new connector vs a sidecar) is the central decision.
2. The Telegram media leg needs a C++ library (`ntgcalls`) via FFI — a new
   dependency class for this project.
3. It forces a second WhatsApp client (whatsapp-rust) to coexist with, or
   eventually replace, the whatsmeow connector.
4. Codec/clock mismatch (WhatsApp 16 kHz ↔ Telegram 48 kHz) requires a
   resampling/transcoding stage with real CPU and latency cost.
5. Account-suspension risk on unofficial clients placing/answering calls
   (whatsapp-rust's own README warns of this).

## Current state facts the design must respect

**The connector protocol is a discrete-message contract and cannot carry media.**
The canonical contract is a bidirectional gRPC `OpenSession` stream plus a
`MediaTransport` service (`docs/connector-protocol.md:3-4`). `ClientFrame` /
`ServerFrame` oneofs carry only message/reaction/receipt/dispatch events
(`crates/wuphf-proto/client.proto:114-126`, `crates/wuphf-proto/server.proto`).
`MediaTransport` streams **blob attachments** by `media_id` before an event is
submitted (`docs/connector-protocol.md:61-68`) — it is a store-and-forward
broker with SQLite persistence and orphan pruning, the opposite of a real-time
path. **Call *signaling*** (ring / answer / hangup / "call started/ended")
*could* be modelled as new frames on this protocol; **call *media* cannot** and
must ride a separate transport.

**Current network topology (three parties, one plane today).** The core is the
gRPC **server**: it binds `listen` (`config/example.toml:1`, default
`127.0.0.1:50051`) and optionally requires an `authorization: Bearer <token>`.
**Connectors are the clients — they dial the core** (`core_address`,
`connectors/whatsapp/config.example.toml:1`) and open the bidirectional
`OpenSession` + `MediaTransport` streams over that one TCP connection. Each
connector separately holds its platform client library (whatsmeow / gotd) and
talks to the **platform's own servers directly** over the platform protocol. So
today there is exactly one topology:

```text
Platform servers  ←platform proto→  Connector  ─dials, TCP gRPC→  Core (server)
```

The core never touches the platform network; the connector never accepts
inbound connections. A call feature must decide how a **media plane** (direct
UDP to platform relays) attaches to this without breaking either invariant —
see the dedicated networking step below.

**The dispatch pipeline's guarantees are wrong for media.** Durable
pending/blocked/acknowledged/dead-lettered state that survives restart and
redelivers on reconnect (README "Durable dispatch state") is exactly what a
live call must NOT do — a dropped call is over, not redelivered. Any call
plumbing must bypass `Storage` dispatch settlement entirely.

**WhatsApp media capability (new): `vendor/whatsapp-rust`.**
- Call control: `voip().call(&peer)`, `accept(&incoming)`, `reject()`,
  `terminate()` (`src/client/voip.rs:85-123`).
- Encoded boundary: exchange complete raw Opus packets over `async_channel`,
  paced every 60 ms; `AudioFormat` fixes codec/PT/RTP-clock
  (`agent_docs/voip_audio_codecs.md`). Cargo features `voip-encoded` /
  `voip-mlow` / `voip-libopus` / `voip`.
- Events: `CallEvent` enum incl. `RelayAllocated`, `ForeignAudio(Bytes)`,
  `AudioFormatMismatch`, video state, RTCP (`wacore/src/voip/engine.rs:363+`).
- It is a **complete standalone WhatsApp client** (messaging too), so running
  it means pairing a **second linked device** (WhatsApp allows ~4) unless the
  existing whatsmeow connector is eventually retired in its favor.

**Telegram: gotd does signaling only; media needs ntgcalls.** The Telegram
connector is `gotd/td` (`docs/connector-protocol.md` Connectors §); nothing in
`connectors/telegram` touches `phone.*` call RPCs today. gotd owns the single
authenticated MTProto session and would drive `phone.requestCall` /
`phone.acceptCall` / `phone.confirmCall` + the DH key exchange, then hand the
negotiated key and relay params to [`ntgcalls`](https://github.com/pytgcalls/ntgcalls)
(C++/WebRTC, raw PCM 16-bit/48 kHz in-out) for the media. **One Telegram login**
— ntgcalls has no session of its own; the pattern mirrors pytgcalls (media
engine driven by a separate MTProto client) and tg2sip.

**Audio bridging is a transcode, not a passthrough.** WhatsApp production
default is 16 kHz mono, PT 120, 60 ms MLOW/Opus; ntgcalls delivers PCM 48 kHz.
The bridge must resample (and, on the encoded WhatsApp path, decode/encode Opus)
in both directions.

**Capability advertising already exists.** `ConnectorCapabilities {
supports_threads, supports_topic_create }`
(`crates/wuphf-model/src/connector.rs:12-14`; wire `common.proto:18`) is the
established pattern for a new `supports_calls`-style flag, following the same
model as plan 015's `supports_reactions`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Confirm vendored VoIP surface | `ls vendor/whatsapp-rust/src/voip vendor/whatsapp-rust/wacore/src/voip` | audio/driver/session/transport + engine/rtp/srtp/relay |
| Read call control API | `sed -n '1,140p' vendor/whatsapp-rust/src/client/voip.rs` | `call/accept/reject/terminate` signatures |
| Read encoded-audio contract | `cat vendor/whatsapp-rust/agent_docs/voip_audio_codecs.md` | formats, features, 60 ms cadence |
| Run the reference PoC | `cd vendor/whatsapp-rust/examples/voip-cli && cargo run --help` | a working live-call CLI to study |
| Survey Telegram call libs | upstream: `pytgcalls/ntgcalls`, `foobar26/tg2sip` | C API surface, PCM format, licensing |
| Locate protocol surfaces | `grep -rn "oneof\|MediaTransport" crates/wuphf-proto/` | where signaling frames would be added |

## Scope

**In scope**: the design document `plans/024-realtime-call-forwarding-design.md`
produced by the steps below, plus an optional throwaway PoC on an unmerged
branch (goal: prove one audio frame crosses WhatsApp→Telegram end to end).

**Out of scope**: any merged code; proto changes; a production call subsystem;
video calls (audio only for v1 — note whatsapp-rust *has* video in-tree, defer
it); group calls.

## Steps

### Step 1: Platform capability + licensing survey

For each leg, record in the design doc, citing concrete upstream symbols:
- **WhatsApp (whatsapp-rust)**: exact call-control signatures; the encoded vs
  PCM audio contract and which cargo feature set to depend on; every
  `CallEvent` the bridge must handle (esp. terminate/mismatch); how an
  *incoming* call surfaces (event type + how to answer). MIT license terms.
- **Telegram (gotd + ntgcalls)**: the `phone.*` RPC sequence gotd must drive
  for outgoing and incoming P2P calls and the DH key exchange; the ntgcalls C
  API entry points for feeding/reading PCM and its build/link model (prebuilt
  binaries vs source); its license and how it enters a Rust or Go process (FFI
  vs cgo). Confirm ntgcalls supports **P2P private calls** (not only group
  voice chats).

### Step 2: Decide where the call subsystem lives (the central decision)

Evaluate and recommend one topology, with trade-offs:
- **(A) In-core Rust subsystem**: whatsapp-rust compiled into a wuphf crate;
  Telegram call media via a Rust↔ntgcalls FFI. Pros: one process, direct
  channel wiring; Cons: pulls a second WhatsApp client and C++ into the core.
- **(B) New standalone "call bridge" connector/sidecar**: a separate binary
  holding whatsapp-rust + ntgcalls, talking to the core only for
  signaling/notifications over an extended connector protocol. Pros: isolates
  the real-time path and the risky clients from the core; Cons: still needs its
  own Telegram + WhatsApp auth story.
- **(C) Hybrid**: existing connectors keep messaging + emit call *signaling*
  frames; a dedicated media daemon owns audio only.

State explicitly: media never touches `Storage`/dispatch;
signaling/notification *may* ride the connector protocol as new frames.

### Step 3: Specify the signaling + notification model

Independent of media, decide what the core and the mirror thread should see:
- New `ClientFrame` events (`CallStarted`/`CallEnded`/`CallMissed`?) and
  whether a call maps onto the existing endpoint/conversation identity model so
  it lands in the right mirror thread.
- A `supports_calls` capability on `ConnectorCapabilities` + `common.proto`
  (mirror plan 015's capability-gating guidance).
- The always-available fallback: even if live audio is deferred, emit a
  synthetic system message ("📞 Incoming WhatsApp call from X" / "Missed call,
  rang 30 s") through the normal pipeline — this is shippable on its own and
  should be called out as a **Phase 1** deliverable.

### Step 4: Specify the networking model (platform ↔ media host ↔ core)

This is the question the current design leaves open, and it constrains Step 2.
Separate the traffic into **three planes** and specify each:

- **Control/signaling plane (core ↔ media host)**: TCP gRPC, reusing the
  existing model where the *connector/media host dials the core* (never the
  reverse) and authenticates with the bearer token. Decide whether call
  signaling (`CallStarted/Ended/Missed`, answer/decline commands) rides the
  existing `OpenSession` stream as new frames, or whether the media daemon
  opens its own session. **No audio bytes cross this plane** — it carries only
  events and lifecycle commands.
- **Platform media plane (media host ↔ platform relays)**: whichever process
  hosts whatsapp-rust / ntgcalls opens **direct UDP sockets to the WhatsApp
  (SRTP/WARP relay) and Telegram (WebRTC/UDP relay) servers**. This traffic
  **never transits the core** — the core is not a media proxy and must not
  become one. Document the consequences: that process needs outbound UDP
  egress and NAT traversal (STUN is already in `wacore/src/voip/stun.rs`;
  ntgcalls brings its own ICE), and it, not the core, bears the bandwidth.
- **Internal audio plane (WhatsApp leg ↔ Telegram leg inside the bridge)**: if
  both legs live in one process (topology A), this is in-process
  `async_channel` frame passing — no sockets. If they live in separate
  processes (topology B/C variants), specify the local transport (UDS / gRPC
  stream / shared memory) and its added latency budget.

Produce a concrete diagram per candidate topology from Step 2 showing which
process owns each UDP media socket, which process dials which over TCP, and
where the transcode runs. Call out explicitly: does the core ever see or route
audio? (Target answer: never.) Where does each platform authentication /
session live relative to the media sockets?

### Step 5: Specify the media path

- The audio graph both directions: WhatsApp encoded-Opus/MLOW (16 kHz, 60 ms,
  PT 120) ↔ resample/transcode ↔ ntgcalls PCM (48 kHz). Name the resampler and
  whether the WhatsApp `voip-libopus` or `voip-mlow` feature is used, and where
  transcode CPU lands.
- Call lifecycle correlation: WhatsApp incoming call → originate Telegram call
  to the owner → on Telegram accept, `accept()` the WhatsApp call → start
  pumping → on either `terminate`/hangup, tear down the other leg. Define the
  state machine and every teardown edge (mismatch, relay-allocate-failed,
  timeout).
- The **two-WhatsApp-clients** consequence: does whatsapp-rust run as a second
  linked device alongside whatsmeow, or is retiring the Go WhatsApp connector a
  prerequisite? Recommend a path and its migration cost.

### Step 6: Throwaway PoC (optional but strongly recommended)

On an unmerged branch, wire `vendor/whatsapp-rust/examples/voip-cli` audio to a
minimal ntgcalls harness (or, if ntgcalls integration is too heavy for a spike,
loop WhatsApp audio to a file/speaker) to prove: (a) an incoming WhatsApp call
can be answered from Rust, (b) decrypted audio frames arrive, (c) the
16 kHz↔48 kHz resample produces intelligible audio. Record findings; delete the
branch.

### Step 7: Implementation roadmap + open questions

End the doc with ordered phases, each independently shippable where possible:
- **Phase 1 — Call *notifications* (no media)**: whatsapp-rust (or even a
  whatsmeow signaling upgrade) emits `CallStarted/Ended/Missed` → system
  message into the mirror thread. Ships value immediately; validates the
  signaling frames and capability flag.
- **Phase 2 — WhatsApp call audio to a sink**: answer + capture decrypted audio
  to prove the media plane in-tree.
- **Phase 3 — Telegram call leg via ntgcalls**: place/answer a Telegram P2P
  call from gotd + move PCM through ntgcalls.
- **Phase 4 — Full bidirectional bridge**: correlate lifecycles + the transcode
  pump both directions.
- **Phase 5 (deferred)** — video.

List every unresolved question with a recommended answer (e.g. "which side may
originate?", "what happens on simultaneous calls?", "ring timeout policy",
"how to present the Telegram-side call so the owner knows who's really
calling").

## Done criteria

- [ ] `plans/024-realtime-call-forwarding-design.md` exists answering Steps 1-7
- [ ] Every platform-API claim cites a concrete upstream symbol / file
- [ ] The subsystem-topology decision (Step 2) is stated with trade-offs and a
      recommendation
- [ ] It is explicit that media bypasses `Storage`/dispatch and that only
      signaling may ride the connector protocol
- [ ] The three network planes (control gRPC / platform-media UDP / internal
      audio) are specified, with a per-topology diagram showing which process
      owns each media socket and confirming the core never routes audio
- [ ] The two-WhatsApp-clients consequence and ntgcalls FFI/build model are
      documented with a recommended path
- [ ] A Phase-1 (notifications-only) shippable slice is identified
- [ ] No merged code changes (`git status` clean apart from `plans/`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- `ntgcalls` turns out not to support Telegram **P2P private** calls (only
  group voice chats) — the Telegram leg's value proposition changes; report
  before designing around it.
- whatsapp-rust cannot run as a second linked device concurrently with the
  existing whatsmeow session (forces an all-or-nothing WhatsApp connector
  replacement) — report; this reprioritizes the whole plan.
- Answering/placing a call in the PoC triggers an account block — report
  immediately; the risk assessment gates whether this feature ships at all.

## Maintenance notes

- Follow plan 015's guidance: land the eventual implementation behind a
  per-connector `supports_calls` capability, extending `ConnectorCapabilities`
  (`crates/wuphf-model/src/connector.rs:12-14`) and `common.proto` Capabilities.
- Phase 1 (call notifications) is decoupled from all the risky media work and
  could be pulled forward independently if live audio stalls.
- Keep `vendor/whatsapp-rust` pinned; its VoIP surface is pre-1.0 (v0.6.x) and
  the encoded-audio API may move. Record the pinned commit in the design doc.
