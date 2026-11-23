# Plan 045: Disappearing-message timers as chat profiles, and whatsapp-rust chat titles

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Build limits (the owner's laptop)**: prefix every cargo command with
> `nice -n 19` and set `CARGO_BUILD_JOBS=2`. Check and test only the crates you
> touch (`-p`), and run `cargo test --workspace` and clippy once, at the end.
> Go: prefix every `go` command with `nice -n 19` and set `GOMAXPROCS=2
> GOFLAGS=-p=2`; build, vet and test one module at a time. The whatsapp-rust
> connector (`connectors/whatsapp-rust`) is its own Cargo workspace, needs
> `RUSTC_BOOTSTRAP=1`, and is **the heaviest build in the repo**: build it only
> in Step 5 (one `cargo test`, then one `cargo clippy`), never alongside
> another build. No release builds, no `nix flake check`, no dev-bin installs.
> Only one agent builds at a time.
>
> **Drift check (run first)**: `git diff --stat 4dd923a..HEAD -- crates/wuphf-proto crates/wuphf-model crates/wuphf-storage crates/wuphf-core crates/wuphf-runtime crates/wuphf-connector crates/wuphf-settings migrations proto/go connectors/shared/coreclient connectors/whatsapp connectors/whatsapp-rust docs config/example.toml README.md`
> Expected: empty. What must still hold (re-check each; if any is false, STOP):
> - The newest migration is `migrations/0018_connector_settings_declarations.sql`
>   (migrations live at the repo root, embedded by
>   `crates/wuphf-storage/src/sqlite.rs:23`).
> - `crates/wuphf-proto/client.proto`: `MessageCreateEvent` (line 12) ends at
>   `sender_name = 11` (line 23); `ClientFrame` ends at `owner_notice = 10`
>   (line 155); `NoticeKind` ends at `NOTICE_KIND_RECOVERED = 4` (line 128).
>   `server.proto`: `MessageCreateDispatch` (line 36) ends at `sender_name = 12`
>   (line 48). `common.proto:18-21`: `Capabilities` has only
>   `supports_threads = 1` and `supports_topic_create = 2`.
> - `crates/wuphf-storage/src/lib.rs:332-346`: `Storage` is the sum of
>   `TopologyStore + MessageStore + MediaStore + DispatchStore +
>   ProvisioningStash + ObservationStore`.
> - `crates/wuphf-runtime/src/connector.rs:127`: the hub refuses a
>   registration that is not equal to the configured `Connector`
>   (capabilities included).
> - `crates/wuphf-core/src/router.rs:114` and `crates/wuphf-core/src/send.rs:93`
>   are the only places that build `Action::Send`.
> - `crates/wuphf-core/src/notify.rs:74`: `notice_is_new` keeps one
>   `NoticeKind` per connector and drops a repeat of it.
> - `connectors/whatsapp-rust/src/normalize.rs:124` sends `chat_title: None`;
>   `connectors/whatsapp-rust/src/main.rs:110-111` hardcodes
>   `supports_threads: false, supports_topic_create: false`.
> - `connectors/whatsapp/internal/whatsapp/connector.go:91-115`: the event
>   handler handles only `*events.Message` and `*events.Receipt` (plus session
>   notices). `connectors/whatsapp/go.mod:16` pins whatsmeow
>   `v0.0.0-20260525123251-933deb5f2ee9`; `connectors/whatsapp-rust/Cargo.toml`
>   pins whatsapp-rust `rev = "5ca80c5c"`.

## Status

- **Priority**: P1 — contacts see the bridge's messages flagged "sent from an
  older version of WhatsApp, will not disappear"
- **Effort**: L
- **Risk**: MED — touches the wire contract, storage (a new table), routing
  and both WhatsApp connectors; every change is additive
- **Depends on**: — (044 is done)
- **Category**: bug / feature
- **Planned at**: commit `4dd923a`, 2026-10-31

## Why this matters

In a WhatsApp chat with disappearing messages on, every message the bridge
sends (the owner replying from Telegram) shows an exclamation mark: "sent from
an older version of WhatsApp, will not disappear". Neither WhatsApp connector
sets `ContextInfo.expiration`; whatsmeow never sets it on its own, and
whatsapp-rust only does when the caller asks. The case that must work: a new
contact whose account-wide default timer is on writes to the owner, and the
owner replies from Telegram.

Second, smaller bug: the Rust WhatsApp connector sends `chat_title: None`
(`connectors/whatsapp-rust/src/normalize.rs:124`), so a new Telegram topic is
named with the raw chat id.

This is slice 1 of 3. Slice 2 (plan 046: chat profiles carry titles,
description, avatar and participants; notes in mirror topics; an info card)
and slice 3 (plan 047: two-way pin / mute / archive sync) build on what this
plan adds. Names here are "chat profile" and "chat update", never "ephemeral
cache", so those slices extend the types instead of reshaping them.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The disappearing timer is a generic, platform-neutral chat property kept by the **core**: a `chat_profiles` table keyed by (connector id, chat id) with a JSON payload, so later fields need no migration. Field `disappearing: Option<DisappearingTimer { seconds: u32 (0 = off), setting_timestamp: Option<i64> }>`; `None` means never learned. Behind a storage role trait, with an SQLite migration, in-memory parity, and a migration test. |
| 2 | Contract: a new client frame `ChatUpdateEvent` carrying a batch of chat updates (history sync reports hundreds of chats at once); `MessageCreateEvent` gains the timer observed on that message; `MessageCreateDispatch` gains the target chat's timer, filled by the core; `Capabilities` gains `supports_disappearing_timers`. |
| 3 | The core updates profiles from create events and chat updates. Conflict rule: when both the stored and the incoming timer carry a setting timestamp, the newer timestamp wins; otherwise the later arrival wins. **Unknown fill (owner, 2026-10-31):** an inbound create that carries no timer, from a connector that declares `supports_disappearing_timers`, into a chat whose profile has `disappearing: None`, records `seconds: 0` with no setting timestamp. It never overrides a known timer, and since it has no timestamp any later report replaces it. The router fills the target timer. No notes in mirror threads in this slice: what is first learned is a silent baseline. |
| 4 | Connectors stamp the timer they receive and persist nothing. When a dispatch carries no timer and the target is a WhatsApp group, the connector looks the group up, uses its timer and reports it as a chat update. When it is still unknown (a DM), it sends without a timer and raises an owner notice — once per chat per process, until a timer for that chat becomes known. |
| 5 | Content sends carry the timer: text (a bare `conversation` is promoted to `extendedTextMessage`), media, stickers, every part of a multi-attachment send. Edits, revokes and reactions do not. `ephemeralSettingTimestamp` is set when known; `disappearingMode` is not. An existing reply `ContextInfo` is merged into, not replaced. |
| 6 | Timer sources each connector reports: (a) inbound `ContextInfo.expiration` on any message, the owner's own included, only when > 0 — absence never turns a timer off; (b) the `EPHEMERAL_SETTING` protocol message (0 = off); (c) group ephemeral change events; (d) history sync conversations, batched. The unknown fill of decision 3 is the core's own rule, not a connector report: for a chat whose timer is known, a message without one still never turns it off. `Event::DisappearingModeChanged` is **not** a source (see Maintenance notes). Chat ids are canonicalized exactly as each connector already canonicalizes `MessageCreateEvent.chat_id`. |
| 7 | whatsapp-rust chat titles land here: groups use the subject, DMs the other party's push name; formatted like the Go connector's `ChatNamer` (`[G]` / `[P]` tags, `formatPersonName`), cached with a TTL. The owner's own messages in a DM (`is_from_me`) send `chat_title: None` — never "Me", never a number fallback — so the core keeps its stored title and a restart causes no rename churn. A DM the owner starts shows the raw id until the other party writes; plan 046's contacts table fixes that (owner, 2026-10-31). The core's `maybe_retitle` (`crates/wuphf-core/src/ingest.rs:149`) renames topics already named with the raw id once a real title arrives. |

### Further decisions (accepted by the owner on 2026-10-31 — do not revisit)

- **The capability is configured on both sides, like `supports_threads`.**
  The hub refuses a registration that differs from the core's `[[connectors]]`
  entry (`crates/wuphf-runtime/src/connector.rs:127`). So the core config gets
  `supports_disappearing_timers` (default `false`), and both WhatsApp connectors
  read the same key from their own config file (default `false`). The example
  configs set it to `true` for WhatsApp. **The owner must set it to `true` in
  the core's `config.local.toml` and in each WhatsApp connector's own config,
  in the same restart** — if only one side has it, the connector is refused
  with "connector registration does not match configuration". Executors never
  touch the local config files.
- **The core only stamps a target whose connector declares the capability.**
  A connector that does not declare it never receives a timer, and a WhatsApp
  connector with the flag off never looks groups up or raises the notice.
  Profiles are still recorded from any connector: recording is cheap and the
  data is right regardless.
- **The timer is stamped when the dispatch is routed** and stored in the
  dispatch (`Action::Send.disappearing`). A dispatch that waits (blocked
  reply, redelivery after a reconnect) carries the value from when it was
  routed. The runtime hub has no storage, so reading the profile at delivery
  time would need a new path through it; not worth it for a timer that rarely
  changes.
- **A new notice kind, `NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN`.** The core
  keeps one health state per connector and drops a repeat of it
  (`notify.rs:74`). Sent as `DEGRADED`, the first unknown-timer notice would
  silence a real "degraded" later, and the next reconnect would announce
  "✅ … is working again". The new kind bypasses that state machine; the
  connector's once-per-chat gate (decision 4) is what limits repeats. The
  `detail` names the chat.
- **The profile is found under any linked form of the chat.** The target's
  chat id is tried first, then every address linked to it
  (`linked_addresses`, the same walk `Core::address_set` does,
  `crates/wuphf-core/src/resolve.rs:125`), preferring the newest timestamp.
  This covers the Go connector's LID/phone forms.
- **History sync reports every conversation**, with `seconds` 0 when the
  conversation carries no `ephemeralExpiration` (a sync snapshot omits the
  field when the timer is off). It is one of the turn-off sources in decision 6.
- **Setting timestamps are Unix seconds.** Where a source has no explicit
  setting timestamp but has an event time (a group notification, an
  `EPHEMERAL_SETTING` message without the field), the event time is used.
  A timer read from a group lookup or from an inbound message without
  `ephemeralSettingTimestamp` has none.
- **The Rust connector reports a DM chat update under both forms** (LID and
  phone number) when it knows the other form. It never sends
  `IdentityMigrated`, so the core has no address links for its chats; without
  this, a timer learned under one form would not reach a chat addressed under
  the other.

## Current state

- **Proto.** `crates/wuphf-proto/{common,client,server}.proto` as listed in
  the drift check. Rust bindings come from `crates/wuphf-proto/build.rs`
  (tonic-build, needs `protoc`). The Go bindings in `proto/go/connector/v1/`
  are committed and generated by hand (headers: `protoc-gen-go v1.36.11`,
  `protoc v7.34.1`; the tools are in the nix dev shell, `flake.nix:35-37`):
  ```sh
  cd crates/wuphf-proto && protoc \
    --go_out=../../proto/go --go_opt=module=github.com/wuphf-rs/wuphf-ai/proto/go \
    --go-grpc_out=../../proto/go --go-grpc_opt=module=github.com/wuphf-rs/wuphf-ai/proto/go \
    common.proto media.proto client.proto server.proto connector.proto functions.proto
  ```
  (`observer.proto` is deliberately not generated for Go; plan 032.) All Go
  modules reach `proto/go` through `replace` directives
  (`connectors/whatsapp/go.mod:5`, `connectors/shared/go.mod:5`).
- **Prost literals that a new proto field breaks** (prost structs need every
  field): `crates/wuphf-connector/src/client.rs:145` (`pb::Capabilities`),
  `crates/wuphf-connector/tests/session.rs:356` (`pb::MessageCreateEvent`),
  `crates/wuphf-runtime/src/grpc.rs:359` (`pb::MessageCreateDispatch`),
  `crates/wuphf-runtime/src/tests.rs:644, 672, 1082`,
  `connectors/whatsapp-rust/src/normalize.rs:119` (`pb::MessageCreateEvent`).
- **Model.** `crates/wuphf-model/src/{message.rs,dispatch.rs,connector.rs}`:
  `NewMessage` (message.rs:160-170), `Action::Send { message, reply }`
  (dispatch.rs:89-92), `ConnectorCapabilities` (connector.rs:12-15),
  `NoticeKind` (connector.rs:30-55), `InboundWork` (connector.rs:63-104).
- **Storage.** Role traits in `crates/wuphf-storage/src/lib.rs`; backends in
  `memory.rs` (one `RwLock<State>`; every table mirrored by a field) and
  `sqlite.rs` (`upsert_json` / `get_json` helpers at 1099-1129 for
  single-key JSON tables). Parity tests in `tests.rs` run one generic
  `async fn x<S: …>(storage: S)` per backend
  (`memory_x` / `sqlite_x`, e.g. `settings_overrides_round_trip`, 980-1023);
  `temp_sqlite()` at 1041. `migration_tests.rs`: `database_before(version)`,
  `assert_stored` compares stored JSON **text** with what today's model
  writes (51-67), so a new field on a stored type must not serialize when it
  is `None`, or every older-shape fixture breaks. The 0018 test (337-372) is
  the model for a migration that only adds a table.
- **Core.** `Core::ingest` (`lib.rs:82`) resolves the origin first and drops
  unmatched chats (`lib.rs:85-87`), so a profile must be recorded before that.
  `Router<S: TopologyStore + MessageStore>` (`router.rs:17-29`) builds every
  routed send in `route` (`router.rs:61-127`); owner messages build their direct
  send in `send.rs:85-98` and fan out through `route_and_record`.
  `Core::notify_owners` (`notify.rs:25-62`). The runtime runs one serial
  ingest worker per connector (`crates/wuphf-runtime/src/lib.rs:204-232`),
  so a connector's chat updates and messages apply in its frame order, and a
  read-merge-write of that connector's profiles needs no transaction.
- **Runtime wire mapping.** `crates/wuphf-runtime/src/grpc.rs`:
  `registration_from_proto` 169-181, `input_from_proto` 183-343 (the
  `OwnerNotice` arm 265-281), `output_to_proto` 345-452, tests from 537.
  `process_work` in `crates/wuphf-runtime/src/lib.rs:240-288`.
- **Rust SDK.** `crates/wuphf-connector/src/client.rs`: `Options` 38-52,
  registration 139-152. **The session loop (164-181) delivers a dispatch and
  drains the connector's input channel in the same `select!`**: a `Sender`
  that awaits `inputs.send(..)` while the channel is full deadlocks. The Go
  `coreclient` loop (`connectors/shared/coreclient/client.go:242-…`) has the
  same shape; `coreclient.SendNotice` (`notice.go:27-33`) is non-blocking for
  that reason.
- **Go WhatsApp connector** (`connectors/whatsapp/internal/whatsapp/`):
  `Normalizer.Handle` (updates.go:84-143) canonicalizes the chat once
  (`n.canonical`, updates.go:77-82, backed by `identity.go:57`) and drops
  protocol messages other than edit and revoke (a non-edit protocol message
  reaches `normalize`, has no text and is dropped). `normalize`
  (updates.go:396-445) sets `ChatTitle` from the namer. `messageContext`
  (updates.go:534-551). Sender: `SendMessage` 77-112, `sendAttachments`
  125-153 (reply context on the first part only), `quotedContext` 203-220,
  `withContext` 307-329 (replaces the media's `ContextInfo`, promotes a bare
  `Conversation`), `textMessage` 503-505. `NewSender` (49) has many test
  callers. Names: `titleTTL` (names.go:16), `tagged` (71-76),
  `formatPersonName` (129-144: saved name; else business name + " (Business)";
  else `push + " (" + jid.User + ")"`; else `jid.User` — no `+`).
  Config: `internal/config/config.go:22-25` (`supports_threads` /
  `supports_topic_create`), `config.example.toml:14-18`.
- **Rust WhatsApp connector** (`connectors/whatsapp-rust/src/`): text only —
  `sender.rs:41-46` dead-letters any dispatch with attachments, and
  `normalize.rs:105-116` drops inbound media. `main.rs:83-86` registers only
  `on_message`. `normalize(ctx)` (normalize.rs:41-137) is synchronous;
  `canonical(jid) = jid.to_non_ad().to_string()` (37-39). `sender_name`
  (143-157) labels own messages "Me". `context_info` (164-199). No tests yet.
  Config: `config.rs` (serde, `#[serde(default)]` for optional keys).
- **whatsmeow** (module cache
  `~/.local/share/golang/pkg/mod/go.mau.fi/whatsmeow@v0.0.0-20260525123251-933deb5f2ee9`):
  `types/group.go:127-131` `GroupEphemeral{IsEphemeral, DisappearingTimer}`
  (embedded in `GroupInfo`, group.go:30); `group.go:593`
  `GetGroupInfo(ctx, jid)`; `types/events/events.go:461-472`
  `events.GroupInfo{JID, Timestamp, Ephemeral *types.GroupEphemeral}` (group.go:921-928:
  `not_ephemeral` gives `IsEphemeral: false`); `events.go:245-247`
  `events.HistorySync{Data *waHistorySync.HistorySync}`.
  `proto/waE2E`: `ContextInfo.Expiration` (field 25, line 9016),
  `EphemeralSettingTimestamp` (26, line 9017); `ProtocolMessage`
  `EphemeralExpiration` / `EphemeralSettingTimestamp` (7482-7483);
  `ProtocolMessage_EPHEMERAL_SETTING = 3` (2723). `proto/waHistorySync`:
  `Conversation.ID`, `EphemeralExpiration` (721), `EphemeralSettingTimestamp`
  (722). History sync is delivered by default: `ManualHistorySyncDownload` is
  false unless set (client.go:105), and `message.go:846-852` downloads every
  notification and dispatches `events.HistorySync` (message.go:692). Nothing to
  enable. The conversation list arrives in the syncs that follow **pairing**;
  a session paired long ago gets no new list until it is re-paired, so the
  history-sync source mainly helps fresh pairings.
- **whatsapp-rust** (checkout
  `~/.cargo/git/checkouts/whatsapp-rust-6e26428647c827f3/5ca80c5`):
  `src/send/mod.rs:186-201` `SendOptions.ephemeral_expiration` (no setting
  timestamp field), applied at 658-666 through
  `wacore/src/proto_helpers.rs:468-500` `set_ephemeral_expiration` (merges into
  an existing `context_info`, promotes a bare `conversation`).
  `src/message/dispatch.rs:19-23` fills `MessageInfo.ephemeral_expiration`
  (`wacore/src/types/message.rs:315`; `MessageInfo` derives `Default`, 298).
  `src/features/groups.rs:94-134` `GroupMetadata{subject (97),
  ephemeral_expiration (121), ..}`, `get_metadata(&jid)` at 417 (always a
  network round trip). `wacore/src/types/events.rs`: `LazyHistorySync` (36),
  `.get()` (154, CPU-heavy; its docs advise `spawn_blocking`), `Event::GroupUpdate`
  (677) with `GroupUpdate{group_jid, timestamp, action}` (1375-1390),
  `Event::HistorySync(Box<LazyHistorySync>)` (708),
  `Event::DisappearingModeChanged` (727, `{from, duration,
  setting_timestamp}` at 601-610; not used, see Maintenance notes), `EventKind` (211). `wacore/src/stanza/groups.rs:173-181`
  `GroupNotificationAction::Ephemeral{expiration, trigger}` (`not_ephemeral`
  parses as `expiration: 0`). `src/bot.rs:903` `on_event_for(&[EventKind], ..)`;
  handlers run on their own tasks, so cross-event order is not guaranteed
  (bot.rs:885-888). History sync is processed unless `skip_history_sync()`
  is called (bot.rs:732, 1182); the connector does not call it.
  `src/client/lid_pn.rs:947` `get_lid_pn_entry(&jid)` →
  `LidPnEntry{lid, phone_number}` (`wacore/src/types/lid_pn.rs:74-81`).
  The proto (`waproto/src/whatsapp.proto`) `ContextInfo` has `expiration = 25`
  and `ephemeralSettingTimestamp = 26`; `Conversation` has `id = 1`,
  `ephemeralExpiration = 9`, `ephemeralSettingTimestamp = 10`, `pnJid = 39`,
  `lidJid = 42` (generated Rust names are snake_case).
- **Docs.** `docs/connector-protocol.md` (session 7-30, client frames 32-54,
  server frames 56-78); `docs/architecture.md` (components table 174-186,
  "Owner Notices" 210-236); `README.md` "What works" (11-38) and the Rust
  connector section (119-136); `config/example.toml:45-48` explains the
  capability keys; `connectors/whatsapp/README.md`.

## Commands you will need

```sh
export CARGO_BUILD_JOBS=2
nice -n 19 cargo check -p wuphf-proto -p wuphf-connector -p wuphf-runtime --all-targets
nice -n 19 cargo test -p wuphf-model
nice -n 19 cargo test -p wuphf-storage
nice -n 19 cargo test -p wuphf-core
nice -n 19 cargo test -p wuphf-runtime
nice -n 19 cargo test -p wuphf-settings
nice -n 19 cargo test -p wuphf-connector
nice -n 19 cargo test --workspace          # once, at the end
nice -n 19 cargo clippy --workspace --all-targets -- -D warnings   # once, at the end
cargo fmt --check

# Go, one module at a time
(cd proto/go && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go build ./... && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go vet ./...)
(cd connectors/shared && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go test ./...)
(cd connectors/whatsapp && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go vet ./... && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go test ./...)
(cd connectors/telegram && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go build ./...)   # final gate only
(cd connectors/instagram && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go build ./...)  # final gate only

# whatsapp-rust: Step 5 only, alone
(cd connectors/whatsapp-rust && RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo test)
(cd connectors/whatsapp-rust && RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --all-targets -- -D warnings)

# Go bindings (needs protoc + plugins: run inside `nix develop`)
(cd crates/wuphf-proto && protoc \
  --go_out=../../proto/go --go_opt=module=github.com/wuphf-rs/wuphf-ai/proto/go \
  --go-grpc_out=../../proto/go --go-grpc_opt=module=github.com/wuphf-rs/wuphf-ai/proto/go \
  common.proto media.proto client.proto server.proto connector.proto functions.proto)
```

## Scope

In: `crates/wuphf-proto/{common,client,server}.proto`, `proto/go/connector/v1/*.pb.go`,
`crates/wuphf-model/src/{lib.rs,chat.rs (new),message.rs,dispatch.rs,connector.rs}`,
`migrations/0019_chat_profiles.sql` (new),
`crates/wuphf-storage/src/{lib.rs,memory.rs,sqlite.rs,tests.rs,migration_tests.rs}`,
`crates/wuphf-core/src/{lib.rs,profile.rs (new),ingest.rs,router.rs,send.rs,notify.rs,resolve.rs,tests.rs}`,
`crates/wuphf-runtime/src/{lib.rs,grpc.rs,tests.rs}` (and any runtime test
literal the new fields break), `crates/wuphf-settings/src/{config.rs,schema.rs}`,
`crates/wuphf-connector/{src/client.rs,tests/session.rs}`,
`connectors/shared/coreclient/{client.go,client_test.go}`,
`connectors/whatsapp/{config.example.toml,README.md,internal/config/*,internal/whatsapp/*}`,
`connectors/whatsapp-rust/{config.example.toml,src/*}`,
`config/example.toml`, `docs/connector-protocol.md`, `docs/architecture.md`,
`README.md`, `plans/README.md`.

Out: notes in mirror topics, chat titles/avatars/participants in profiles
(046); pin / mute / archive (047); media in the Rust connector; showing chat
updates in the observation log or TUI; asking WhatsApp for a DM's timer (no
API used here); `disappearingMode` in sent messages; `.env.local`,
`config.local.toml`, `*.session.json`, `.claude/`.

## Git workflow

Commit directly on `v2`, one commit per step, with the message given in the
step. Do not push. Do not create branches.

## Steps

### Step 0: Baseline

Run the drift check. Then `nice -n 19 cargo test --workspace` → record the
count. Record `go test ./...` results for `connectors/shared` and
`connectors/whatsapp` (one at a time). Do **not** build whatsapp-rust here.

### Step 1: The contract

`crates/wuphf-proto/common.proto` (after `Capabilities`, which gains a field):

```proto
message Capabilities {
  bool supports_threads = 1;
  bool supports_topic_create = 2;
  // The connector stamps a chat's disappearing-messages timer on what it
  // sends, and reports the timers it observes. The core fills
  // MessageCreateDispatch.disappearing only for connectors that declare it.
  bool supports_disappearing_timers = 3;
}

// A chat's disappearing-messages timer, a platform-neutral chat property.
// `seconds` 0 means off. `setting_timestamp` is when the timer was set, in
// Unix seconds, when the platform says; the core keeps the newer of two
// timestamped reports, and otherwise the later one.
message DisappearingTimer {
  uint32 seconds = 1;
  optional int64 setting_timestamp = 2;
}
```

`client.proto`:

```proto
message MessageCreateEvent {
  … // fields 1-11 unchanged
  // The chat's timer as this message carried it (WhatsApp: the message's
  // ContextInfo.expiration, on the owner's own messages too). Set only when the
  // message carries one greater than 0. A message without one never turns a
  // known timer off; when the chat's timer is unknown and the connector
  // declares supports_disappearing_timers, the core records it as off.
  optional DisappearingTimer disappearing = 12;
}

// One chat's properties as the platform reported them. An absent field was not
// reported and leaves what the core knows unchanged. Later plans add fields
// (title, pins, mutes) here.
message ChatUpdate {
  string chat_id = 1;
  optional DisappearingTimer disappearing = 2;
}

// Chat properties changed or were learned (a timer setting message, a group
// notification, a history sync). A batch, since a history sync reports
// hundreds of chats at once. `chat_id`s are canonical, exactly as the
// connector reports them in MessageCreateEvent.
message ChatUpdateEvent {
  repeated ChatUpdate updates = 1;
}
```

`NoticeKind` gains:

```proto
  // Not about the connector's health: a message went to a chat whose
  // disappearing-messages timer the connector could not learn, so it was sent
  // without one and will not disappear. `detail` names the chat. The core does
  // not fold it into the connector's health state.
  NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN = 5;
```

`ClientFrame` gains `ChatUpdateEvent chat_update = 11;`.

`server.proto`, `MessageCreateDispatch` gains:

```proto
  // The target chat's disappearing-messages timer as the core knows it: absent
  // when never learned, `seconds` 0 when known to be off. Set only for
  // connectors that declare supports_disappearing_timers. The connector puts
  // it on every part of the send; edits, deletes and reactions carry none.
  optional DisappearingTimer disappearing = 13;
```

Keep the Rust workspace compiling with neutral values at each prost literal
listed under "Current state" (`supports_disappearing_timers: false`,
`disappearing: None`). The `OwnerNotice` arm in `grpc.rs:266-276` matches
`pb::NoticeKind` exhaustively: add `Ok(pb::NoticeKind::DisappearingTimerUnknown)`
to its fallback arm for now (Step 4 maps it properly). Also add
`disappearing: None` at
`connectors/whatsapp-rust/src/normalize.rs:119`; that crate is not built until
Step 5, so note it in the commit body.

Regenerate the Go bindings with the `protoc` command above (inside `nix
develop`; first check `protoc --version` prints `libprotoc 34.1` and
`protoc-gen-go --version` prints `v1.36.11`).

**Verify**:
- `git status --short proto/go` → exactly `client.pb.go`, `common.pb.go`,
  `server.pb.go` modified; nothing else changed or added.
- `nice -n 19 cargo check -p wuphf-proto -p wuphf-connector -p wuphf-runtime --all-targets` → exit 0.
- `proto/go` build + vet → exit 0; `connectors/whatsapp` `go vet ./...` → exit 0.

Commit: `feat(proto): chat updates and disappearing timers in the connector contract`.

### Step 2: Chat profiles in the model and in storage

New `crates/wuphf-model/src/chat.rs` (re-export from `lib.rs` like the other
modules; extend the module list in the crate doc):

```rust
//! What the core knows about a platform chat, apart from routing: its chat
//! profile, and the updates a connector reports about it.

/// A chat's disappearing-messages timer. `seconds` 0 means off.
/// `setting_timestamp` is when it was set (Unix seconds), when known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisappearingTimer {
    pub seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting_timestamp: Option<i64>,
}

/// Everything the core knows about one platform chat on one connector, keyed
/// by its native address. Fields are optional and default to unknown, so a
/// later field needs no migration: stored rows simply lack it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatProfile {
    pub connector_id: ConnectorId,
    pub chat_id: String,
    /// `None` until a timer is learned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<DisappearingTimer>,
}

impl ChatProfile {
    pub fn new(connector_id: ConnectorId, chat_id: impl Into<String>) -> Self { … }
}

/// A connector's report about one chat. `None` fields were not reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUpdate {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<DisappearingTimer>,
}
```

`crates/wuphf-storage/src/lib.rs`: a role trait, added to the `Storage` sum
(the router reads it on every send) and to the blanket impl:

```rust
/// What the core knows about each platform chat, keyed by connector and the
/// chat's native id (threads are not part of the key).
#[async_trait]
pub trait ChatProfileStore: Send + Sync + 'static {
    async fn chat_profile(&self, connector_id: &ConnectorId, chat_id: &str)
        -> StorageResult<Option<ChatProfile>>;
    /// Insert or replace the profile.
    async fn put_chat_profile(&self, profile: &ChatProfile) -> StorageResult<()>;
}
```

`migrations/0019_chat_profiles.sql`:

```sql
-- What the core knows about each platform chat apart from routing (plan 045):
-- a JSON ChatProfile keyed by the connector and the chat's native id. New
-- profile fields go in the JSON and need no schema change.
CREATE TABLE chat_profiles (
    connector_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (connector_id, chat_id)
);
```

`sqlite.rs`: an `INSERT … ON CONFLICT(connector_id, chat_id) DO UPDATE SET
payload = excluded.payload` and a `SELECT payload … WHERE connector_id = ? AND
chat_id = ?` (the `upsert_json` helper takes one key column, so write these two
queries directly). `memory.rs`: `chat_profiles: HashMap<(ConnectorId, String),
ChatProfile>` in `State`, with a doc comment naming the table it mirrors.

Tests:
1. `crates/wuphf-storage/src/tests.rs`: `memory_chat_profiles_round_trip` /
   `sqlite_chat_profiles_round_trip` over a generic
   `chat_profiles_round_trip<S: ChatProfileStore>`: unknown → `None`; put then
   get → equal; put again with another timer → replaced; the same chat id on
   two connectors stays separate; a profile with `disappearing: None` round
   trips. Plus `sqlite_chat_profiles_survive_reopen` (pattern:
   `sqlite_settings_overrides_survive_reopen`).
2. `crates/wuphf-storage/src/migration_tests.rs`:
   `migration_0019_adds_chat_profiles` — `database_before(19)`, seed the
   sample dispatches with `insert_dispatches` in today's JSON (capture it with
   `print_current_json`), run `MIGRATOR`, `assert_dispatches` (older rows still
   decode), then insert a `chat_profiles` row by SQL with payload
   `{"connector_id":"whatsapp-main","chat_id":"1@s.whatsapp.net","disappearing":{"seconds":86400,"setting_timestamp":1700000000}}`
   and decode it as `ChatProfile`. Add `decode_all::<ChatProfile>(&pool,
   "chat_profiles")` to `real_database_migrates_and_decodes`.
3. `crates/wuphf-model`: `a_profile_without_a_timer_stores_no_timer_key` —
   `serde_json::to_string(&ChatProfile::new(..))` has no `"disappearing"`, and
   `{"connector_id":"c","chat_id":"x"}` decodes with `disappearing: None`.

**Verify**: `nice -n 19 cargo test -p wuphf-model -p wuphf-storage` → green,
Step 0 storage count + the new tests. Mutation: give the SQLite query the wrong
key (drop `connector_id` from the `WHERE`) → the two-connector assertion fails.
Commit: `feat(storage): chat profiles keyed by connector and chat`.

### Step 3: The core keeps profiles and stamps sends

Model changes (each new field `#[serde(default, skip_serializing_if =
"Option::is_none")]` on stored types, so older stored JSON and the migration
fixtures are unchanged — this is a new field, not a rename, so no data
migration and no serde alias):

- `NewMessage.disappearing: Option<DisappearingTimer>` (the timer the message
  carried).
- `Action::Send { message, reply, disappearing: Option<DisappearingTimer> }`
  — the target chat's timer when the send was routed.
- `ConnectorCapabilities.supports_disappearing_timers: bool`
  (`#[serde(default)]`).
- `NoticeKind::DisappearingTimerUnknown`, with `summary()` "sent a message to a
  chat whose disappearing-messages timer it could not learn, so that message
  will not disappear", and `pub fn is_health(self) -> bool` (false only for
  this kind).
- `InboundWork::ChatUpdate { connector_id: ConnectorId, updates: Vec<ChatUpdate> }`
  (doc: "the platform reported chat properties; batched"), covered in
  `connector_id()` and `set_connector()`.

Fix every literal the new fields break with neutral values (`disappearing:
None`, `supports_disappearing_timers: false`): `router.rs:114`, `send.rs:93`,
`grpc.rs:175` and `199`, `wuphf-settings/src/schema.rs:86`, the model's
`actions()` test (dispatch.rs:263), storage `tests.rs` (`send` helper ~703),
`migration_tests.rs` (`sample_dispatches`), core and runtime tests,
`crates/wuphf-connector/tests/session.rs:44, 77`. The wire mapping proper is
Step 4.

New `crates/wuphf-core/src/profile.rs`:

```rust
//! Chat profiles: what the core knows about each platform chat apart from
//! routing. Connectors report what they observe; the core merges reports and
//! hands the result to sends.

/// Merge a reported timer into the stored one, returning the new value when it
/// changes. With a setting timestamp on both sides the newer wins (an equal
/// one counts as newer); otherwise the later report wins. A report that
/// repeats the stored seconds without a timestamp keeps the stored timestamp.
pub(crate) fn merge_disappearing(
    stored: Option<DisappearingTimer>,
    incoming: DisappearingTimer,
) -> Option<DisappearingTimer> {
    let next = match stored {
        None => incoming,
        Some(current) => match (current.setting_timestamp, incoming.setting_timestamp) {
            (Some(known), Some(reported)) if reported < known => return None,
            _ if incoming.seconds == current.seconds => DisappearingTimer {
                seconds: current.seconds,
                setting_timestamp: incoming.setting_timestamp.or(current.setting_timestamp),
            },
            _ => incoming,
        },
    };
    (stored != Some(next)).then_some(next)
}

/// Apply one report to a profile; true when anything changed.
pub(crate) fn apply(profile: &mut ChatProfile, update: &ChatUpdate) -> bool { … }

impl<S: Storage> Core<S> {
    /// Record chat properties a connector reported, returning how many chats
    /// changed. Runs on the connector's serial ingest worker, so a
    /// read-merge-write cannot race another update for the same connector.
    pub async fn update_chats(&self, connector_id: &ConnectorId, updates: Vec<ChatUpdate>)
        -> Result<usize, CoreError> { … }
}

/// The timer a send to `target` carries: `None` when the target's connector
/// does not declare supports_disappearing_timers, or when no profile of the
/// chat (under its own id, then under any linked form, newest timestamp
/// first) has one.
pub(crate) async fn target_disappearing<S: TopologyStore + ChatProfileStore>(
    storage: &S,
    routing: &RoutingConfig,
    target: &Target,
) -> Result<Option<DisappearingTimer>, StorageError> { … }
```

- Move the link walk out of `Core::address_set` (`resolve.rs:125-146`) into
  `pub(crate) async fn address_forms<S: TopologyStore>(storage, connector_id,
  address)`; `Core::address_set` calls it, and so does
  `target_disappearing` (with `ChatAddress::new(chat_id, None)`).
- `Core::ingest` (`lib.rs:82`): before `resolve_origin`, when the payload is
  `EventKind::NewMessage` with `disappearing: Some(timer)`, record
  `ChatUpdate { chat_id: source.address.chat_id, disappearing: Some(timer) }`
  for `source.connector_id`. This runs for chats no rule matches, which is
  what lets a later reply find the timer.
- **Unknown fill** (decision 3), in the same place: when the payload is
  `EventKind::NewMessage` with `disappearing: None`, the source connector
  declares `supports_disappearing_timers` (`routing.connector(..)` →
  `capabilities`), and the stored profile is missing or has `disappearing:
  None`, store `DisappearingTimer { seconds: 0, setting_timestamp: None }`.
  Do it directly (read, check `None`, write), not through
  `merge_disappearing`: the merge rule would let a timestamp-less 0 replace a
  known timer, which the fill must never do. Put it in one helper,
  `Core::observe_message_timer(&self, routing, source, timer: Option<DisappearingTimer>)`,
  so both cases sit together.
- `Router`: bound `S: TopologyStore + MessageStore + ChatProfileStore`; in
  `route`, `disappearing: target_disappearing(&*self.storage, &self.routing,
  &target).await?` (after `resolve_target`). `send.rs`: the direct owner
  dispatch gets the same, using its `routing` snapshot.
- `notify.rs`: `notify_owners` skips `notice_is_new` when `!kind.is_health()`;
  `notice_text` stays as it is (⚠️ marker, connector named, detail appended).
- `crates/wuphf-runtime/src/lib.rs` `process_work`: an arm for
  `InboundWork::ChatUpdate` that calls `core.update_chats(..)`, logs
  `debug!(connector, updates, changed, "chat profiles updated")`, and returns
  no dispatches.

Tests (`crates/wuphf-core/src/tests.rs`, with the existing WhatsApp/Telegram
fixtures; give the WhatsApp connector `supports_disappearing_timers: true`
where a test needs stamping):

1. `merge_disappearing_follows_the_setting_timestamp` — table test over the
   pure function: unknown + report → report; newer ts wins; older ts ignored
   (`None`); equal ts with other seconds wins; no ts on either side → later
   wins; same seconds without ts keeps the stored ts and reports no change;
   `0` replaces a running timer.
2. `a_message_with_a_timer_records_the_chat_profile` — ingest a WhatsApp
   create with `disappearing: Some(86400)` in a chat no rule matches →
   `Ignored(Unmatched)` and the profile holds 86400.
3. `chat_updates_apply_in_order_and_count_changes` — `update_chats` with a
   batch of three chats (one repeating what is stored) → returns 2.
4. `a_reply_into_a_chat_with_a_timer_carries_it` — WhatsApp DM create with a
   timer (mirrored to Telegram), then a Telegram reply in that topic → the
   dispatch to WhatsApp is `Action::Send { disappearing: Some(timer), .. }`;
   the Telegram dispatch carries `None` (Telegram lacks the capability).
5. `a_chat_known_to_be_off_carries_zero` — profile `seconds: 0` → the send
   carries `Some(seconds: 0)`, not `None`.
6. `a_target_without_the_capability_gets_no_timer` — same as 4 with the
   capability off → `None`.
7. `a_timer_learned_under_a_linked_form_is_found` — record the timer under the
   phone form, link phone ↔ LID with `migrate_identity`, route to the LID
   endpoint → the timer is carried.
8. `owner_messages_carry_the_target_timer` — `send_owner_message` to a
   WhatsApp chat with a profile → the direct dispatch carries the timer.
9. `timer_notices_bypass_the_health_state` — `notify_owners(Degraded)`, then
   `DisappearingTimerUnknown` twice, then `Degraded` again: the timer notices
   both go out, and the second `Degraded` is still suppressed as a repeat
   (state untouched by the timer notices); a `Recovered` after only timer
   notices says nothing.
10. `a_message_without_a_timer_fills_an_unknown_chat_with_off` — capable
    WhatsApp connector, no profile, create with `disappearing: None` → profile
    `Some(seconds: 0, setting_timestamp: None)`; a later Telegram reply
    carries `Some(seconds: 0)`.
11. `a_message_without_a_timer_never_overrides_a_known_one` — profile 86400
    (with and without a setting timestamp), then a create without a timer →
    still 86400, timestamp unchanged.
12. `no_fill_from_a_connector_without_the_capability` — the same create from a
    connector that does not declare `supports_disappearing_timers` → no
    profile written (`chat_profile` is `None`).
13. `a_reported_timer_replaces_the_fill` — after the fill, a create carrying
    604800 without a timestamp → 604800.

**Verify**: `nice -n 19 cargo test -p wuphf-model -p wuphf-storage -p
wuphf-core -p wuphf-runtime -p wuphf-settings -p wuphf-connector` → green.
Mutations: drop the `reported < known` guard → test 1 fails; stamp
`disappearing: None` in `route` → test 4 fails; skip the profile write in
`ingest` for unmatched chats (move it after `resolve_origin`) → test 2 fails;
route the fill through `merge_disappearing` → test 11 fails; drop the
capability check from the fill → test 12 fails.
Commit: `feat(core): keep chat profiles and stamp the target's disappearing timer on sends`.

### Step 4: Wire mapping, configuration and the Rust SDK

- `crates/wuphf-runtime/src/grpc.rs`:
  - `registration_from_proto`: map `supports_disappearing_timers`.
  - `input_from_proto`: `MessageCreate` maps `event.disappearing` into
    `NewMessage.disappearing`; a new `ChatUpdate` arm builds
    `InboundWork::ChatUpdate` (each `chat_id` through `required_string`); the
    `OwnerNotice` arm maps `DisappearingTimerUnknown`.
  - `output_to_proto`: `Action::Send { disappearing, .. }` →
    `MessageCreateDispatch.disappearing`.
  - Two small helpers, `timer_from_proto` / `timer_to_proto`.
- `crates/wuphf-settings/src/config.rs:181-184`: `#[serde(default)] pub
  supports_disappearing_timers: bool` on the connector entry, documented;
  `schema.rs:86` maps it.
- `config/example.toml`: `supports_disappearing_timers = true` on
  `whatsapp-main` and on the commented `whatsapp-rust` entry; extend the
  comment at 45-48: the key lets the core stamp a chat's disappearing timer on
  sends to this connector, and must match the connector's own config.
- `crates/wuphf-connector/src/client.rs`: `Options.supports_disappearing_timers`
  (documented like the other two), sent in the registration.

Tests:
1. In `grpc.rs` tests: `chat_update_frames_become_inbound_work` (two updates,
   one without a timer); `a_create_event_carries_its_timer`;
   `a_send_carries_the_target_timer_to_the_wire` (Some and None);
   `the_timer_notice_kind_maps_to_its_own_kind`;
   `registration_maps_the_disappearing_capability`.
2. `crates/wuphf-runtime/src/tests.rs`, end to end through the hub (pattern:
   the existing tests that register WhatsApp and Telegram sessions): register
   WhatsApp with the capability (and configure it so), send a `ChatUpdateEvent`
   then a Telegram reply into a mirrored DM → the `MessageCreateDispatch` the
   WhatsApp session receives has `disappearing.seconds == 604800`.
3. `crates/wuphf-connector/tests/session.rs`: the registration frame carries
   `supports_disappearing_timers` from `Options`.

**Verify**: `nice -n 19 cargo test -p wuphf-runtime -p wuphf-settings -p
wuphf-connector` → green. Mutation: map `disappearing: None` in
`output_to_proto` → test 1 (`a_send_carries…`) and test 2 fail.
Commit: `feat(runtime): carry chat updates and disappearing timers over the wire`.

### Step 5: whatsapp-rust connector

Build this crate **once at the end of the step** (`cargo test`, then
`cargo clippy`), alone. Write the code so the pure parts are unit-testable
without a network client.

1. **Config** (`config.rs`): `#[serde(default)] pub
   supports_disappearing_timers: bool`. `config.example.toml`: set it `true`
   with a comment that it must match the core's `[[connectors]]` entry.
   `main.rs`: pass it to `wuphf_connector::Options`.
2. **`groups.rs` (new): a group directory.** `GroupDirectory { client:
   Arc<Client>, entries: Mutex<HashMap<String, (GroupFacts, Instant)>> }` with
   `GroupFacts { subject: String, disappearing_seconds: u32 }`, a 5-minute TTL
   (`GROUP_TTL`, documented like Go's `titleTTL`), `async fn facts(&self, jid:
   &Jid) -> Option<GroupFacts>` (calls `client.groups().get_metadata(jid)` on
   a miss or an expired entry; a failure returns `None` and caches nothing),
   and `fn forget(&self, jid)`. One directory is shared by the namer and the
   sender.
3. **`names.rs` (new): chat titles, mirroring `connectors/whatsapp/internal/whatsapp/names.go`.**
   Pure helpers, unit-tested: `tagged(kind, title) -> Option<String>`
   (`None` for an empty title) and `format_person_name(number: &str, push:
   &str) -> String` (`"{push} ({number})"`, or `number` alone; no `+`, as in
   Go; saved address-book names are not available through this client, so a
   saved contact shows its push name and number where Go shows the saved
   name). `ChatTitles { groups: Arc<GroupDirectory>, client: Arc<Client> }` with `async fn
   title(&self, info: &MessageInfo) -> Option<String>`:
   - group chat → `tagged("G", subject)` from the directory;
   - DM, not from me → number = the chat's phone number (for a LID chat,
     `client.get_lid_pn_entry(&chat)` → `phone_number`, else the JID user);
     `tagged("P", format_person_name(number, info.push_name.trim()))`;
   - DM from me (`info.source.is_from_me`) → `None`. The push name on my own
     message is mine, not the chat's, and any fallback (a cache, the number)
     would rename the topic after a restart. The core keeps the title it has
     stored; a DM the owner starts shows the raw id until the other party
     writes (046's contacts table fixes that);
   - anything else (newsletter, broadcast) → `None`.
   Keep the DM rule in a pure `fn dm_title(is_from_me: bool, number: &str,
   push: &str) -> Option<String>` so it is tested without a client. No
   per-person cache is needed (the push name rides on every message); only
   group subjects are cached, in the directory.
4. **`normalize.rs`.** Make `normalize` pure over its parts:
   `fn normalize(info: &MessageInfo, message: &wa::Message, chat_title:
   Option<String>) -> Option<pb::ClientFrame>` (`sender_name` takes `info`).
   `Normalizer` gains `titles: Arc<ChatTitles>` and `handle` resolves the
   title first (`let title = self.titles.title(&ctx.info).await;`), then calls
   `normalize(&ctx.info, &ctx.message, title)`. In `normalize`:
   - `MessageCreateEvent.chat_title = chat_title`;
     `disappearing = message_timer(info, message)`: `info.ephemeral_expiration`
     when > 0, with `setting_timestamp` from the message's
     `context_info.ephemeral_setting_timestamp` when present and > 0;
   - a `protocol_message` of type `EPHEMERAL_SETTING` → a `ChatUpdateEvent`
     with one update: `seconds = ephemeral_expiration.unwrap_or(0)`,
     `setting_timestamp = ephemeral_setting_timestamp` or else
     `info.timestamp.timestamp()`. Add this arm before the `_ => None` at
     normalize.rs:83. The handler then also emits the alternate form (item 5's
     `forms`) for a DM.
5. **`chats.rs` (new): chat events.** `ChatEvents { inputs, client, groups }`
   with `async fn handle(&self, event: Arc<Event>)`, registered in `main.rs`
   with `.on_event_for(&[EventKind::GroupUpdate, EventKind::HistorySync], ..)`
   (not `DisappearingModeChanged`; see Maintenance notes):
   - `GroupUpdate` with `GroupNotificationAction::Ephemeral { expiration, .. }`
     → one update for `canonical(&group_jid)`, `seconds = expiration`,
     `setting_timestamp = timestamp.timestamp()`. Every `GroupUpdate` also
     calls `groups.forget(&group_jid)` (the subject or timer may have changed).
   - `HistorySync(lazy)` → decode on `tokio::task::spawn_blocking` (clone the
     `Arc<Event>` into the closure, call `.get()`), map every conversation
     whose id parses as a user, LID or group JID to an update (`seconds =
     ephemeral_expiration.unwrap_or(0)`, `setting_timestamp` when present and
     > 0), plus the `pn_jid` / `lid_jid` form when present and different; send
     in frames of at most `CHAT_UPDATE_BATCH = 500`.
   - `forms(&self, jid) -> Vec<String>`: `canonical(jid)`, plus for a user or
     LID JID the other form from `client.get_lid_pn_entry` when known.
   Keep the mapping from a decoded `wa::HistorySync`, a `GroupUpdate` action
   and a protocol message to `pb::ChatUpdate`s in pure functions (in
   `disappearing.rs`, below) so they are tested without a client.
6. **`disappearing.rs` (new): extraction and stamping, pure, plus the send-side
   fallback.**
   - `fn stamp(message: &mut wa::Message, timer: Option<&pb::DisappearingTimer>)`:
     nothing for `None` or `seconds == 0`; otherwise
     `message.set_ephemeral_expiration(seconds)` (`MessageExt`; promotes a bare
     `conversation` and keeps an existing reply `context_info`), then set
     `ephemeral_setting_timestamp` on the same `context_info` when known
     (`SendOptions` has no field for it, `send/mod.rs:186-201`, so it is set
     on the message; do not also pass `SendOptions.ephemeral_expiration`).
   - `TimerFallback { groups: Arc<GroupDirectory>, inputs:
     mpsc::Sender<pb::ClientFrame>, warned: Mutex<HashSet<String>> }` with
     `async fn resolve(&self, chat_id: &str, chat: &Jid, dispatched:
     Option<pb::DisappearingTimer>) -> Option<pb::DisappearingTimer>`:
     dispatched `Some` → clear `chat_id` from `warned` and return it; a group →
     `groups.facts(chat)`; on success report a `ChatUpdateEvent` for `chat_id`
     (no timestamp) and return the timer; otherwise (a DM, or the lookup
     failed) insert `chat_id` into `warned` and, only if it was not there,
     emit `OwnerNotice { kind: DisappearingTimerUnknown, detail: "chat
     {chat_id}" }`; return `None`.
   - **Never await the input channel from the sender**: the session loop that
     runs `send_message` is the one draining it (`client.rs:164-181`). Use
     `inputs.try_send(frame)`, and on `Full` hand the frame to
     `tokio::spawn(async move { let _ = inputs.send(frame).await; })`.
7. **`sender.rs`.** `WhatsAppSender::new(client, fallback: Option<TimerFallback>)`
   (`None` when the config flag is off). In `send_message`, after building the
   text or reply message: `let timer = match &self.fallback { Some(f) =>
   f.resolve(&dispatch.target_chat_id, &chat, dispatch.disappearing).await,
   None => dispatch.disappearing };` then `stamp(&mut message, timer.as_ref())`.
   `edit_message` and `delete_message` stay as they are. `main.rs` builds one
   `Arc<GroupDirectory>`, the `ChatTitles`, `ChatEvents`, and the fallback
   (from `inputs_tx.clone()`).

Tests (`#[cfg(test)] mod tests` in each new module; `MessageInfo::default()`
and hand-built `wa::Message`s, no client):
- `names`: `format_person_name` with and without a push name; `tagged("G",
  "")` is `None`; `tagged("P", "Alice (1555)")` is `"[P] Alice (1555)"`;
  `dm_title(false, "1555", "Alice")` is `Some("[P] Alice (1555)")`;
  `dm_title(true, "1555", "Owner")` is `None` (never "Me", never the owner's
  push name, never the number).
- `normalize`: a text message with `ephemeral_expiration = Some(86400)` and a
  context timestamp → the event carries both; `Some(0)` or `None` → no timer;
  the title passed in lands in `chat_title`, and `None` stays `None`;
  `EPHEMERAL_SETTING` with 604800 and with 0
  (and without its timestamp → falls back to the message time) → a
  `ChatUpdateEvent`.
- `disappearing`: `stamp` on `wa::Message::text("hi")` → an
  `extended_text_message` with `expiration` and `ephemeral_setting_timestamp`;
  on `text_with_context` → `stanza_id` kept and the timer added; `seconds 0`
  and `None` → unchanged; the group-update mapping for `Ephemeral{604800}` and
  `Ephemeral{0}`; the history-sync mapping (absent expiration → 0, a status
  broadcast skipped, the `pn_jid` form added, 1201 conversations → frames of
  500 / 500 / 201).
- `TimerFallback` gate logic: factor the "warn once until known" set into a
  small `NoticeGate` with `fn first_warning(&self, chat) -> bool` and `fn
  known(&self, chat)`, and test it: first → true, again → false, after
  `known` → true again.

**Verify**: `(cd connectors/whatsapp-rust && RUSTC_BOOTSTRAP=1
CARGO_BUILD_JOBS=2 nice -n 19 cargo test)` → green; then clippy (`-D
warnings`) clean; `git diff --stat connectors/whatsapp-rust/Cargo.lock` →
empty (no dependency change). Mutation: make `stamp` skip
`ephemeral_setting_timestamp` → the stamp test fails.
Commit: `feat(whatsapp-rust): disappearing timers and chat titles`.

### Step 6: Go WhatsApp connector

1. **Capability.** `connectors/shared/coreclient/client.go`:
   `Options.SupportsDisappearingTimers bool`, sent in the registration
   (client.go:214-215). `connectors/whatsapp/internal/config/config.go`:
   `SupportsDisappearingTimers bool toml:"supports_disappearing_timers"`,
   documented; `config.example.toml`: `supports_disappearing_timers = true`
   with a comment that it must match the core. `connector.go:127-134`: pass
   it in `coreclient.Options`.
2. **Normalizer** (`updates.go`):
   - `messageTimer(message *waE2E.Message) *wuphfpb.DisappearingTimer` from
     `messageContext`: `GetExpiration()` > 0, with
     `GetEphemeralSettingTimestamp()` when > 0; `normalize` sets
     `event.Disappearing`.
   - `ephemeralSetting(message *events.Message) (*wuphfpb.DisappearingTimer,
     bool)`: a `ProtocolMessage` of type `EPHEMERAL_SETTING`; `Seconds =
     GetEphemeralExpiration()`, `SettingTimestamp =
     GetEphemeralSettingTimestamp()` or else `message.Info.Timestamp.Unix()`.
     In `Handle`, right after the `SecretEncryptedMessage` block
     (updates.go:108-116): when it matches, send one `ChatUpdate` for `chat`
     (already canonical) and return.
   - `sendChatUpdates(ctx, updates []*wuphfpb.ChatUpdate) error`: frames of at
     most `chatUpdateBatch = 500` updates.
   - `HandleGroupInfo(ctx, evt *events.GroupInfo) error`: nothing when
     `evt.Ephemeral == nil`; else one update for `n.canonical(ctx,
     evt.JID)`, `Seconds = DisappearingTimer` when `IsEphemeral` else 0,
     `SettingTimestamp = evt.Timestamp.Unix()` when the time is set.
   - `HandleHistorySync(ctx, evt *events.HistorySync) error`: for every
     `evt.Data.GetConversations()` whose `GetID()` parses to a
     `types.DefaultUserServer`, `types.HiddenUserServer` or
     `types.GroupServer` JID: chat `n.canonical(ctx, jid)`, `Seconds =
     GetEphemeralExpiration()`, `SettingTimestamp` when
     `GetEphemeralSettingTimestamp() > 0`; batched.
   - `connector.go` event switch: `case *events.GroupInfo:` and `case
     *events.HistorySync:` calling the two handlers (errors flow to
     `eventErrors` like messages).
   Chat ids: always through `n.canonical` (identity.go:57), exactly as
   `normalize` builds `MessageCreateEvent.ChatId` (updates.go:416); a different
   form would key the profile under a chat id the core does not route.
3. **Sender** (`sender.go`, plus a new `disappearing.go`):
   - `disappearing.go`: `GroupInfoClient` interface (`GetGroupInfo(ctx,
     types.JID) (*types.GroupInfo, error)`); `timerSource { groups
     GroupInfoClient; inputs chan<- *wuphfpb.ClientFrame; life
     context.Context; mu sync.Mutex; warned map[string]struct{} }`;
     `func (t *timerSource) For(ctx, chatID string, chat types.JID,
     dispatched *wuphfpb.DisappearingTimer) *wuphfpb.DisappearingTimer` with
     the same rules as the Rust `TimerFallback.resolve` (dispatched wins and
     clears the warning; a group is looked up, reported and used; otherwise a
     `NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN` notice with detail `"chat " +
     chatID`, once until known). Frames go out without blocking:
     `select { case t.inputs <- f: default: go func() { select { case
     t.inputs <- f: case <-t.life.Done(): } }() }` — the coreclient loop that
     calls `SendMessage` is the one draining `inputs`.
   - `withDisappearing(message *waE2E.Message, timer
     *wuphfpb.DisappearingTimer) *waE2E.Message`: unchanged when `timer` is nil
     or `GetSeconds() == 0`; otherwise get-or-create the `ContextInfo` of the
     content (`ExtendedTextMessage`, `ImageMessage`, `DocumentMessage`,
     `VideoMessage`, `AudioMessage`, `StickerMessage`; a bare `Conversation` is
     promoted to `ExtendedTextMessage`, as `withContext` does) and set
     `Expiration` and, when present, `EphemeralSettingTimestamp`.
   - `Sender` gains `timers *timerSource` set through `func (s Sender)
     WithTimers(t *timerSource) Sender` (so `NewSender` and its test callers
     are unchanged). `SendMessage`: after `target`, `timer := dispatch.GetDisappearing()`;
     when `s.timers != nil`, `timer = s.timers.For(ctx,
     dispatch.GetTargetChatId(), chat, timer)`. Apply `withDisappearing`
     **after** `withContext` (which replaces media `ContextInfo`) on the single
     message, and on **every** part in `sendAttachments` (pass the timer in).
     `EditMessage` / `DeleteMessage` unchanged.
   - `connector.go`: `NewSender(..).WithTimers(newTimerSource(client, inputs,
     ctx))` only when `c.cfg.SupportsDisappearingTimers`.

Tests:
- `updates_test.go`: `TestNormalizeCarriesTheMessageTimer` (expiration and ts;
  0 → nil; an own message carries it too); `TestHandleReportsEphemeralSetting`
  (604800 with ts; 0 = off; missing ts → message time; the chat id is the
  canonical one under `fakeIdentity`); `TestHandleGroupInfoReportsEphemeral`
  (on, off, and no `Ephemeral` → no frame); `TestHandleHistorySyncBatches`
  (1201 conversations → 3 frames, 500 / 500 / 201; absent expiration → 0; a
  `status@broadcast` id skipped; ids canonicalized).
- `sender_test.go`: `TestSendStampsTheTimerOnText` (a bare text becomes an
  `ExtendedTextMessage` with `Expiration` and `EphemeralSettingTimestamp`);
  `TestSendStampsTheTimerOnEveryAttachmentKind` (image, file, video, audio,
  sticker, and over-length text sent as a file);
  `TestSendStampsEveryPartOfAMultiAttachmentSend` (three images → three
  messages with `Expiration`; reply context still only on the first);
  `TestSendMergesTheTimerIntoAReplyContext` (`StanzaID` and `Participant`
  kept); `TestSendWithAnOffOrMissingTimerSetsNoExpiration`;
  `TestEditAndDeleteCarryNoTimer`.
- `disappearing_test.go`: `TestTimerSourcePrefersTheDispatchedTimer`;
  `TestTimerSourceLooksUpAGroupAndReportsIt` (fake `GroupInfoClient`, a
  `ChatUpdateEvent` frame on `inputs`, the group's timer returned; off group →
  `Seconds: 0`); `TestTimerSourceWarnsOncePerChatUntilKnown` (unknown DM →
  one notice; again → none; a dispatched timer, then unknown again → a notice);
  `TestTimerSourceNeverBlocksOnAFullInputChannel` (unbuffered channel, no
  reader → `For` returns).
- `internal/config/config_test.go`: the key parses and defaults to false.
- `connectors/shared/coreclient/client_test.go`: the registration carries
  `SupportsDisappearingTimers` (pattern:
  `TestRegistrationCarriesTheSettingsDeclaration`).

**Verify**: `connectors/shared` `go test ./...` → green; `connectors/whatsapp`
`go vet ./...` and `go test ./...` → green. Mutation: stamp only the first part
in `sendAttachments` → `TestSendStampsEveryPartOfAMultiAttachmentSend` fails;
make `withDisappearing` replace the `ContextInfo` → the reply-merge test
fails.
Commit: `feat(whatsapp): disappearing timers on sends and from chat events`.

### Step 7: Docs

- `docs/connector-protocol.md`: Session — the third capability and that it
  must match the core config. Client frames — `ChatUpdateEvent` (batched,
  canonical ids, absent fields leave the core's value alone);
  `MessageCreateEvent.disappearing` (only when > 0; never turns a known timer off;
  the core records an unknown chat as off when a capable connector reports a
  message without one);
  the new `OwnerNotice` kind, which the core does not fold into health state.
  Server frames — `MessageCreateDispatch.disappearing` (absent = unknown, 0 =
  off; put on every part of a content send, never on edits / deletes /
  reactions). Fix the stale `delivery { … }` sentence in the same bullet only
  if it is still there (it describes options that moved to the connectors).
- `docs/architecture.md`: the `wuphf-storage` row — seven role traits,
  including chat profiles; a short "Chat Profiles" section after "Replicated
  Operations" (keyed by connector and native chat id, merged by the
  setting-timestamp rule, read when a send is routed, recorded before routing
  so unrouted chats count, the extension point for 046/047); in "Owner
  Notices", one sentence that the unknown-timer notice is not a health state.
- `README.md`: a "What works" bullet for disappearing timers (both WhatsApp
  connectors) and the Rust connector's chat titles; the Rust connector section
  mentions `supports_disappearing_timers` in both configs.
- `connectors/whatsapp/README.md`: one bullet for disappearing timers.

**Verify**: `grep -n 'supports_disappearing_timers' docs/connector-protocol.md
config/example.toml connectors/whatsapp/config.example.toml
connectors/whatsapp-rust/config.example.toml` → a hit in each.
Commit: `docs: chat profiles and disappearing timers`.

### Step 8: Gate and index

- `nice -n 19 cargo test --workspace` → Step 0 count + the new tests, green;
  clippy (`-D warnings`) and `cargo fmt --check` clean.
- Go: `proto/go` build + vet, then `connectors/shared` test, `connectors/whatsapp`
  vet + test, `connectors/telegram` and `connectors/instagram` build — one at a
  time. Do not rebuild whatsapp-rust unless Step 6 or 7 touched it.
- `plans/README.md`: row 045 DONE with a notes bullet: the migration number,
  test counts, the mutation checks, deviations, and **the owner action**:
  set `supports_disappearing_timers = true` for each WhatsApp connector in
  the core's `config.local.toml` and in that connector's own config, in the
  same restart.
- Commit: `docs(plans): mark plan 045 done`.

## Test plan

| Test | Where | Proves |
|---|---|---|
| `chat_profiles_round_trip` (memory, sqlite), `sqlite_chat_profiles_survive_reopen` | wuphf-storage | backend parity, keying, durability |
| `migration_0019_adds_chat_profiles` | wuphf-storage | the table exists; older dispatch rows still decode |
| `a_profile_without_a_timer_stores_no_timer_key` | wuphf-model | later fields need no migration |
| `merge_disappearing_follows_the_setting_timestamp` | wuphf-core | the conflict rule |
| `a_message_with_a_timer_records_the_chat_profile` | wuphf-core | recorded before routing |
| `a_message_without_a_timer_fills_an_unknown_chat_with_off`, `a_message_without_a_timer_never_overrides_a_known_one`, `no_fill_from_a_connector_without_the_capability`, `a_reported_timer_replaces_the_fill` | wuphf-core | the unknown fill |
| `chat_updates_apply_in_order_and_count_changes` | wuphf-core | batch ingest |
| `a_reply_into_a_chat_with_a_timer_carries_it`, `a_chat_known_to_be_off_carries_zero`, `a_target_without_the_capability_gets_no_timer`, `a_timer_learned_under_a_linked_form_is_found`, `owner_messages_carry_the_target_timer` | wuphf-core | stamping on routed and owner sends |
| `timer_notices_bypass_the_health_state` | wuphf-core | notices do not corrupt health dedup |
| grpc mapping tests, end-to-end hub test, SDK registration test | wuphf-runtime, wuphf-connector | the wire |
| names / normalize / disappearing / gate tests | whatsapp-rust | titles, timer extraction, stamping |
| updates / sender / disappearing / config / coreclient tests | Go | every source, every content type, reply merge, notice once per chat |

## Done criteria

- [ ] Proto, Go bindings (only the three files changed), and both Rust and Go
      sides agree on the new fields.
- [ ] Migration 0019 plus its test; parity tests on both backends.
- [ ] Every test above passes; every listed mutation fails a test.
- [ ] Workspace tests, clippy, fmt clean; Go modules green; whatsapp-rust
      built once, green, lock unchanged.
- [ ] Docs updated; row 045 DONE with notes and the owner action.

## STOP conditions

- The drift check shows changes, or a "must still hold" fact is false.
- `protoc` / `protoc-gen-go` versions differ from the committed headers, or
  regenerating changes any `proto/go` file other than `client.pb.go`,
  `common.pb.go`, `server.pb.go`.
- A library fact used here is not where the plan says (e.g. whatsapp-rust has
  no `set_ephemeral_expiration` or
  `GroupNotificationAction::Ephemeral` at rev `5ca80c5c`; whatsmeow's
  `events.GroupInfo` has no `Ephemeral`).
- Building whatsapp-rust needs a dependency change (its `Cargo.lock` would
  move).
- A migration fixture test (0011, 0013, 0014, 0016) fails after Step 3: a new
  field is serializing when `None`.
- Any test that existed at Step 0 starts failing for a reason this plan does
  not explain.

## Maintenance notes

- **Owner action before the feature works**: `supports_disappearing_timers =
  true` on both sides (core `[[connectors]]` and the connector's own file) for
  each WhatsApp connector, in one restart; a mismatch makes the core refuse the
  connector.
- **Owner notices are rare by design.** Any inbound message from a capable
  connector settles an unknown chat (with its timer, or as off), so the
  unknown-timer notice fires only for a chat the bridge sends into before it
  has seen a single message there since the profile table existed — typically
  a DM the owner starts from Telegram. Re-pairing brings a full history sync,
  which fills every chat at once.
- **Why `DisappearingModeChanged` is not a source** (whatsapp-rust
  `Event::DisappearingModeChanged`, a contact's account-wide default): that
  default applies only to chats started after it changes, never to an
  existing chat's timer. And the first message in a new chat already carries
  `ContextInfo.expiration`, which decision 6(a) reports. Mapping it onto the
  existing DM would stamp a timer on a chat that has it off.
- **Order of reports.** Messages and chat updates from one connector apply in
  frame order, but the Rust client runs its event handlers on separate tasks,
  so a group notification and a message can reach the connector's input
  channel in either order. The timestamp rule makes that harmless whenever
  both carry a setting timestamp.
- **Stale timers on waiting dispatches**: a send keeps the timer it was
  routed with. If that ever matters, stamp at delivery instead (the hub needs
  storage access for that).
- **Rust DM titles**: the owner's own DM messages carry no title, so the core
  keeps whatever it stored and a restart renames nothing. A DM the owner
  starts from the phone is provisioned with the raw chat id and retitled when
  the contact first writes; plan 046's contacts table names it from the start.
- Slices 046 and 047 add fields to `ChatProfile`, `ChatUpdate` and the
  `ChatUpdate` proto message; keep them optional with `skip_serializing_if`
  so stored profiles need no migration.
