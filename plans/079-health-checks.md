# Plan 079: Health checks: probes of rule chats, health pauses that hold messages, tools, notices, a Health page and `wuphf-server health`

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `CARGO_BUILD_JOBS=2`, `nice`; never build on battery
> (`/sys/class/power_supply/AC/online` = 0). No local `nix build` of packages:
> check a vendorHash with `.#<pkg>.goModules` and let CI build.

## Status

- **Priority**: P1
- **Effort**: L
- **Depends on**: botconf 018 (data pages, `v0.17.0`) for the Health page;
  everything else does not wait for it.
- **Category**: core / connectors / settings panel

## Why

A misconfigured rule fails silently. A Telegram target group that is not a
forum, or an account that may not create topics there, makes every topic
creation fail. The dispatch is dead-lettered with a log line, and the owner
never hears of it. The same happens when an account has left a source group,
when a WhatsApp group is admin-only, or when mediaconv has no `ffmpeg`.

## Design decisions (grilled with the owner, 2026-11-25)

| # | Decision |
|---|---|
| 1 | Checks: rule targets (exists, member, can post, is a forum, can create topics), rule sources (exists, member), wiring (a connector not connected for 15 min, a configured function no connected provider offers, a wuphf-misc chat no connector knows), dead-lettered dispatches, and the external tools each process needs. |
| 2 | **The core decides.** Connectors report facts about chats, and processes report their own tools; the core evaluates rules against them. |
| 3 | Facts come from a **probe** the core sends over the connector session (like `RunCommand`). Every connector answers: Telegram, both WhatsApp connectors, Instagram. A connector that does not answer, or answers late, is **unknown**. Unknown never pauses and never resumes. |
| 4 | Checks run when a connector registers, when the rules change (panel, CLI, reload), and on demand (Run checks, `wuphf-server health --run`). There is no timer, and a failed send does not trigger a probe. |
| 5 | A failed **chat-fact** check **pauses** only the affected pairs, in the broken direction only. Wiring, tools and secondary rights (pin, delete others' messages) only warn. |
| 6 | Messages for a paused direction are **held** and delivered in order on resume. A held message older than **7 days** is dropped, and the resume notice counts them. |
| 7 | A pause ends by itself when a re-check passes. **Ignore** on a problem resumes it and keeps that check quiet until it passes once, after which the ignore lapses. |
| 8 | A health pause is core state, not a setting. It is listed next to plan 076's manual pauses, each with its reason. A manual pause wins (plan 076 drops messages). |
| 9 | Results surface as an owner notice when a problem appears and ✅ when it clears (one per problem, never per retry), on a `/wuphf` Health page (botconf 018), and in `wuphf-server health`. That command prints the stored results, works while the core is down and exits non-zero on open problems; `--run` checks through the running core. Dead letters are listed on the page and in the CLI only, with no notice. |
| 10 | Tools: each process reports its own, when it registers and when it is probed. A missing tool warns. A provider stops offering a function it cannot run, so callers fall back at once. Version skew, disk space and clock checks are out of scope. |

## Design

### Directions and what each needs

A rule replicates both ways. **Forward** runs source → destination, and
**back** runs destination → source (replies typed in a topic). For each rule:

- **Destination**, needed for forward:
  - exists, member, can send;
  - for `ManyToOne`, and for `OneToOne` with a thread that is created, also
    *forum* and *can create topics*;
  - can pin and can delete others' messages are warnings only.
- **Source**, needed for back: exists, member, can send. It is probed when
  the rule names chats: `OneToOne`'s chat, or `ManyToOne`'s `only` list. A
  `ManyToOne` over every chat probes nothing on the source side.
- **Pause keys**: `(rule, direction, chat)`, where `chat` is the destination
  container, or the source chat for back. A bad destination container pauses
  forward for every conversation of that rule. A bad source chat pauses back
  for that chat's conversation only.

### Facts on the wire

```proto
// server.proto, ServerFrame
ProbeChats probe = 11;
message ProbeChats { string request_id = 1; repeated string chat_ids = 2; }

// client.proto, ClientFrame
ProbeResult probe_result = 13;
message ProbeResult {
  string request_id = 1;
  repeated ChatFacts chats = 2;
  repeated ToolStatus tools = 3;
}
message ChatFacts {
  string chat_id = 1;
  Fact exists = 2; Fact member = 3; Fact can_send = 4;
  Fact forum = 5; Fact can_create_topics = 6;
  Fact can_pin = 7; Fact can_delete_others = 8;
  string detail = 9;          // the platform's words, for the notice
}
enum Fact { FACT_UNKNOWN = 0; FACT_YES = 1; FACT_NO = 2; }
message ToolStatus { string name = 1; bool available = 2; string needed_for = 3; }
// Registration.tools = 6, ProviderHello.tools = 3
```

The core waits 20 s for the answer, then treats the probe as unknown. A
connector answers whatever it can; a fact it cannot learn is `FACT_UNKNOWN`.

### Core

- **`wuphf-health` crate (pure).** It takes the routing config, the facts per
  connector and chat, the tools per process, connector liveness and the
  ignores, and returns `Vec<Problem>`. A `Problem` has:
  - `key`, stable, e.g. `target-not-forum:<rule>:<connector>:<chat>`;
  - `severity`: `Pause { rule, direction, connector, chat }` or `Warn`;
  - `text`.
  
  It holds no I/O. It is tested table by table.
- **Storage, behind a new `HealthStore` role** (migration 0022):
  - `health_problems (key, severity, payload, since_ms, notified)`;
  - `health_ignores (key)`;
  - the dispatch state `held`, with the `held_at_ms` column, via a table
    rebuild for the state CHECK.
- **Runtime `HealthMonitor`:**
  - It sends probes over live sessions (like `ConnectorCommands`) on
    register, on a rules change and on Run, and stores the facts.
  - It re-evaluates, stores the diff and sends a notice for each new problem
    and ✅ for each cleared one, through `notify_owners`.
  - Resuming releases the held dispatches.
- **Holding:** when the core routes a dispatch to a paused
  `(rule, direction, chat)`, it stores the dispatch `held` instead of sending
  it. Thread provisioning is held the same way, so no topic creation is
  attempted. Releasing sends the held dispatches in creation order and
  dead-letters those older than 7 days with the reason `held over 7 days`.
- **Functions:** a configured function that no connected provider declares
  fails at once (`UNAVAILABLE`) instead of waiting out the deadline, and it is
  a wiring warning.

### Connectors

- **Telegram (gotd, the user session):** `channels.getFullChannel` and
  `channels.getParticipant(self)` give exists, member, forum and the admin
  rights (`manage_topics`, `post_messages`, `pin_messages`,
  `delete_messages`). The default banned rights cover non-admins, for
  example "anyone can create topics".
- **WhatsApp, both connectors:** group info gives exists, member, and
  announce, so can send means not announce-only, or the account is an
  admin. A direct chat is always yes.
- **Instagram:** the thread exists and the account can send there.
- **Tools:**
  - The Go WhatsApp connector reports `webpmux`, needed for sticker pack
    metadata.
  - mediaconv reports `ffmpeg` and `magick`/`convert`, and declares
    `media.convert` only when both are found.

### Surfaces

- **Notices:** `⚠️ <text>` once per problem key, and `✅ <text> fixed (N held
  messages sent, M dropped)` when it clears. A new notice kind, `HEALTH`, is
  not folded into the connector's health.
- **Health page** (botconf 018 `DataSource`, "🩺 Health" in `/wuphf`):
  - open problems, each one opening its detail with an Ignore action;
  - paused directions, with the held count;
  - dead letters, newest first, paged;
  - a Run checks action.
- **CLI:** `wuphf-server health [--run] [--address]`. The stored results come
  from the database. `--run` calls the new `HealthTransport.Run` RPC
  (observer token), which returns the problems.

## Steps

1. proto: probe frames, tools, the `HEALTH` notice kind and the
   `HealthTransport` service; Rust and Go bindings.
2. `wuphf-health`, the evaluator, with tests covering every check and
   direction.
3. Storage: migration 0022, `HealthStore` and the `held` state, in both
   backends, with migration tests.
4. Core holding and releasing (router and acknowledge), with tests: holding
   while paused, release in order, dropping after 7 days, a manual pause
   winning.
5. Runtime `HealthMonitor`: probes, triggers, notices, `HealthTransport`, and
   functions failing fast. Tests use a fake connector session.
6. Connectors: probe answers and tools (Telegram, Go WhatsApp,
   whatsapp-rust, Instagram), and mediaconv's tools and functions.
7. CLI `wuphf-server health`.
8. The Health page on botconf 018.
9. Docs: Operations (Health), connector-protocol (Probes), Configuration.
