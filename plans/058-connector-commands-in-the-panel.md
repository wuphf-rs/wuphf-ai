# Plan 058: Connector commands in the settings panel (resync, a fresh link code)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report. When done, update the status row for this plan in
> `plans/README.md`.
>
> **Build limits (the owner's laptop)**: one build at a time
> (`pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv`
> prints nothing first), `CARGO_BUILD_JOBS=2 nice -n 19` for cargo,
> `GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19` for go, only the crates and modules
> touched. The whatsapp-rust workspace needs `RUSTC_BOOTSTRAP=1` and runs
> alone. Stage explicit paths only.
>
> **Drift check (run first)**: botconf `v0.12.0` is tagged (plan 014:
> `Panel::section_actions`, `Panel::on_action`). In this repo:
> `crates/wuphf-runtime/src/settings_auth.rs` builds the one `Panel`
> (`settings_server`); `crates/wuphf-runtime/src/grpc.rs` `open_session`
> spawns the reader and the outgoing task; connectors' sections are
> `connector:<id>` (`connector_settings.rs`, `section_id`); `ServerFrame`
> ends at `chat_state = 9`, `ClientFrame` at `chat_update = 11`,
> `Registration` at `settings = 4`. Either false → STOP.

## Status

- **Priority**: P1 — the owner asked for it ("do the panel")
- **Effort**: L
- **Risk**: MED — a new frame pair on every connector session (additive;
  old connectors never see a command, as they declare none)
- **Depends on**: botconf 014 (`v0.12.0`); plans 056 and 057 (the Rust
  connector declares settings, so it has a section; linking after a logout)
- **Category**: feature
- **Planned at**: `e70aeb5` (`v2`, after 056), 2026-11-23

## Why this matters

The WhatsApp connectors need two owner actions that are not settings:

- **Resync app state**: fetch every app-state collection (contacts, pins,
  mutes, archives) whole again, when names or chat state look stale. Today
  only `--resync-app-state` at startup does it (the Rust connector; the Go
  one has nothing).
- **Send a new link code**: while the connector waits to be linked again,
  link codes are told at growing gaps (5 min … 1 h, plan 057). An owner who
  is ready now should not wait for the next one.

The owner settled (2026-11-22, "all these sound good") that these are
buttons in the Telegram `/wuphf` panel, sent by the core to the connector.

## Design decisions

| # | Decision |
|---|---|
| 1 | **Connectors declare their commands** at registration: `Registration.commands` (repeated `ConnectorCommand { id, title }`). The core decides nothing about them; it offers what the live session declared. |
| 2 | **The panel offers them on the connector's section** (`connector:<id>`, botconf 014's section actions), only while that connector's session is live. A connector with no settings section has nowhere to show them (both WhatsApp connectors declare settings). |
| 3 | **A press is a request on the session**, not a dispatch: `ServerFrame.run_command` (`RunCommand { request_id, command_id }`), answered by `ClientFrame.command_result` (`CommandResult { request_id, ok, message }`). Not stored, not redelivered: a command pressed while the connector is away is refused ("not connected"). |
| 4 | **The core waits up to 10 s** for the result and shows its message as the panel's notice (`❌ …` when not ok or timed out). Connectors answer at once and do slow work in the background. |
| 5 | **SDKs**: a `Commands` handler beside the `Sender` (Rust: `Options.commands: Option<Arc<dyn Commands>>`; Go: `Options.Commands`, an interface), which lists the declarations and runs one by id. Unknown ids answer not ok. |
| 6 | **The WhatsApp commands**, in both connectors: `resync_app_state` ("🔄 Resync app state") — every collection, in the background, on the linked client (not linked → not ok); `fresh_link_code` ("🔗 Send a new link code") — when not linked, the next code (QR or pair code) is told at once whatever the gap (and a pair code prompts the phone); when linked, ok with "already linked". |

## Steps

1. **Proto** (`common.proto`, `server.proto`, `client.proto`), Go bindings
   regenerated (protoc, keeping each file's header). Commit
   `feat(proto): connector commands`.
2. **Core** (`wuphf-runtime`): `connector_commands.rs` — a registry of live
   sessions' commands (attach on session start, detach on its end, a request
   map of oneshots, `run` with the timeout); `grpc.rs` attaches the session,
   merges `RunCommand` frames into the outgoing task and settles
   `CommandResult` frames before `input_from_proto`; `settings_auth.rs` gives
   the panel `section_actions` / `on_action` over the registry;
   `wuphf-cli` wires one registry into both. Tests: offered only while live,
   a run round trip, a timeout, not connected, a stale session's detach not
   removing a newer one, the panel listing and running them (fake session).
   Commit `feat(core): run connectors' commands from the settings panel`.
3. **Rust SDK**: `commands.rs` (`Commands` trait, boxed futures), the
   registration field, the session loop answering `RunCommand` in a task.
   Tests (`tests/commands.rs`, a recording core): the registration carries
   the declarations; a command's result reaches the core; an unknown id
   answers not ok. Commit `feat(connector-sdk): connector commands`.
4. **Go coreclient**: `Options.Commands` (`Commands` interface), the
   registration field, the loop answering in a goroutine. Tests as step 3.
   Commit `feat(coreclient): connector commands`.
5. **whatsapp-rust**: `commands.rs` over `Linking` (a `fresh` flag),
   `LinkedClient` and the current session's `Resync` (`Resync::all`);
   wired in `main.rs`. Tests: linking's fresh flag (QR told at once; pair
   code asked, prompted and told at once; cleared after use); the commands'
   answers linked / not linked. Commit
   `feat(whatsapp-rust): resync and fresh-link-code commands`.
6. **Go whatsapp**: the same two commands (`FetchAppState` for every
   `appstate.WAPatchName`, full sync; the linking flag). Tests as step 5.
   Commit `feat(whatsapp): resync and fresh-link-code commands`.
7. **Docs, gate, dev binaries, index**: README (the panel buttons),
   `docs/connector-protocol.md` (the frames); the gate per component; rebuild
   the server and both WhatsApp dev binaries; row 058 DONE.

## STOP conditions

- The drift check fails.
- A Go renderer needs a change to show the buttons or the notice.
- Any existing test fails for a reason this plan does not explain.

## For the owner

Restart the server and both WhatsApp connectors. In `/wuphf` → the
WhatsApp connector's section: "🔄 Resync app state" (a notice says it
started; renamed contacts and chat state arrive as snapshots), and, while it
waits to be linked, "🔗 Send a new link code" (the notice with the code or
QR arrives within the next code rotation, under a minute).
