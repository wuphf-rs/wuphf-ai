# Plan 047: Two-way pin, mute and archive between WhatsApp chats and Telegram mirror topics

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
> in Step 7 (one `cargo test`, then one `cargo clippy`), never alongside
> another build. No release builds, no `nix flake check`, no dev-bin installs.
> Only one agent builds at a time.
>
> **Drift check (run first)**: plan 046 must have landed. Find its last
> commit with `git log --oneline --grep='mark plan 046 done' -1` (`$P046`),
> then run
> `git diff --stat $P046..HEAD -- crates migrations proto/go connectors/shared/coreclient connectors/whatsapp connectors/whatsapp-rust connectors/telegram docs`.
> Expected: empty. What must still hold (if any is false, STOP):
> - `plans/README.md` rows 045 and 046 are DONE. HEAD at planning time was
>   `4dd923a` (045 and 046 planned, not executed); the facts below are their
>   expected output.
> - `client.proto` `ChatUpdate` ends at `participant_changes = 7`;
>   `ChatUpdateEvent { updates = 1; snapshot = 2; }`; `server.proto`
>   `MessageCreateDispatch` ends at `system_note = 14`; `ServerFrame` ends at
>   `seen_receipt = 8`.
> - `crates/wuphf-model/src/chat.rs` `ChatProfile` has `disappearing, title,
>   description, avatar, participant_count`; `InboundWork::ChatUpdate {
>   connector_id, updates, snapshot }`; `Action::Note` exists.
> - `crates/wuphf-core/src/profile.rs`: `Core::update_chats(connector, updates,
>   snapshot) -> Result<Vec<Dispatch>, CoreError>`, `conversations_of_chat`,
>   `address_forms`; `notes.rs` exists.
> - No migration newer than `0019_chat_profiles.sql` (046 added none).
> - `Cargo.toml:28-30` pins botconf, botconf-grpc and botconf-panel at
>   `tag = "v0.10.0"`.
> - Library pins: whatsmeow `v0.0.0-20260525123251-933deb5f2ee9`, whatsapp-rust
>   `rev = "5ca80c5c"`, gotd `v0.144.0`.

## Status

- **Priority**: P3
- **Effort**: L
- **Risk**: MED — the bridge starts changing chat state on both platforms;
  a wrong comparison could make the two sides fight
- **Depends on**: 046
- **Category**: feature
- **Planned at**: commit `4dd923a`, 2026-10-31 (045 and 046 planned, not
  executed)

## Why this matters

The owner reads WhatsApp through Telegram mirror topics but still organizes
chats on the phone: pinning, muting, archiving. Today none of that crosses the
bridge, so the Telegram side shows every topic unpinned, unmuted and open, and
organizing in Telegram does nothing on WhatsApp. This slice syncs the three
states both ways, per conversation, configurable.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Pin, mute and archive sync both ways and are configurable. |
| 2 | A botconf settings section `chat_state.{pin,mute,archive}`, each `off` / `to_mirror` / `to_source` / `both`, default `both`. A global setting with a per-conversation override, editable in the panel, defined the way plans 039 / 040 / 044 define settings and panel fields. |
| 3 | Mapping to Telegram: pin ↔ pinned forum topic; mute ↔ the topic's notify settings, with WhatsApp's mute-until mapped exactly onto Telegram's `mute_until` ("always" = the maximum, unmute = 0); archive ↔ closed topic. The owner is an admin, so they can still post in a closed topic. |
| 4 | The profile stores the state. Loops are prevented by comparing values against the stored profile: an echo is always a no-op. No suppression window. |
| 5 | Contract: connectors report state changes through `ChatUpdateEvent`; the core sends a new chat-state dispatch to the other side. WhatsApp executes it with whatsmeow app-state builders + `SendAppState` and whatsapp-rust's chat actions; Telegram observes topic pin / mute / close changes and executes the reverse. |
| 6 | Sources: whatsmeow `events.Pin` / `Mute` / `Archive`; whatsapp-rust `PinUpdate` / `MuteUpdate` / `ArchiveUpdate`; history sync chat state as the silent baseline. |

### Further decisions (accepted by owner 2026-10-31 — do not revisit)

- **One state per conversation, kept in the source chat's profile.** The
  Telegram topic has no profile of its own. A report from either side is
  compared with the source chat's stored `pinned` / `mute` / `archived`:
  equal → nothing (this is what makes every echo a no-op); different and the
  direction allows it → store it there and dispatch to the other side;
  different and the direction does not allow it → a WhatsApp-side report is
  still stored (the profile mirrors WhatsApp), a Telegram-side report is
  dropped.
- **Directions are named from the source.** `to_mirror` = WhatsApp →
  Telegram; `to_source` = Telegram → WhatsApp.
- **Baselines.** A history sync, or an app-state full sync
  (`FromFullSync` / `from_full_sync`), is a snapshot (046's
  `ChatUpdateEvent.snapshot`): stored, never dispatched. There is no initial
  alignment of existing topics; the first real change on either side syncs.
- **Mute is one value, `muted_until`** (Unix seconds): `0` not muted, `-1`
  muted forever, otherwise the end. WhatsApp reports milliseconds (a negative
  value means forever, whatsmeow `appstate.go:264-275`); the connector divides
  by 1000. Telegram's `mute_until` is in seconds; `2147483647` (int32 max, the
  value clients use for "forever") ↔ `-1`. A WhatsApp mute set from Telegram
  uses `BuildMuteAbs` / `mute_chat_until` with exactly `muted_until * 1000`,
  so WhatsApp's echo divides back to the same number. Connectors report a
  mute whose end has already passed as `0`, so an expired mute is not a
  difference.
- **Override key.** botconf map entry names cannot contain `.`
  (`botconf::keys::is_entry_name`), and every WhatsApp chat id does
  (`…@s.whatsapp.net`). So an override is a named entry with a required
  `chat` field (a `wuphf.chat` reference, either the source chat or its
  mirror topic), like a rule names its chats:
  `chat_state.chats.<name> = { chat, pin?, mute?, archive? }`; an absent
  field falls back to the global value.
- **No new capability flag.** A connector that does not implement the new
  dispatch acknowledges it unchanged (Go `ackOnly`, Rust trait default), as
  with reactions today. Telegram and both WhatsApp connectors implement it.
  This avoids a third core/connector config pair the owner would have to set.
- **Only mirror topics on Telegram.** A Telegram endpoint without a thread
  (a one-to-one rule's plain chat) receives no chat-state dispatch, and the
  Telegram connector reports only topic state. Dialog pins, folders and peer
  mutes for plain chats are a later plan.
- **Mute needs a Telegram user session.** Notify settings belong to an
  account; a bot session's are meaningless. In bot mode the Telegram
  connector acknowledges the mute part without acting and never reports
  mutes. Pin and close work for an admin bot with the manage-topics right.
- **Archiving on WhatsApp unpins** (`BuildArchive` adds an unpin mutation,
  `appstate/encode.go:93-113`; WhatsApp does the same). The resulting unpin
  report syncs like any change, so the topic is unpinned too.
- **`UpdatePinnedForumTopics`** (a bulk reorder) reports only the topics in its
  `Order` as pinned; an unpin hidden in a reorder is not detected (the
  connector keeps no topic state). Single pins and unpins arrive as
  `UpdatePinnedForumTopic`.

## Current state (after 046)

- **Contract** as in the drift check. `ServerFrame` (`server.proto`) bodies 1-8;
  the Go bindings regenerate with 045's `protoc` command.
- **Core.** `Core::update_chats` (046) applies each `ChatUpdate` to the
  profile under `(connector, chat_id)`, posts notes, and returns dispatches;
  `conversations_of_chat` resolves a chat to its conversations through
  `address_forms` → `resolve_endpoint` → `conversation_for_endpoint`. A
  provisioned topic has its own alias `(connector, chat, thread)`
  (`persist_provisioned_thread`, `acknowledge.rs:212-230`), so
  `resolve_endpoint` resolves a topic to its mirror endpoint.
  `Router::is_mirror` (`router.rs:292-299`).
- **Settings.** `crates/wuphf-settings/src/schema.rs`: `WuphfSchema::settings`
  (124-141) declares `rules` (`RuntimeSetting::per_entry` with the `RULE`
  form) and `notify`; the rule form (144-196) shows `Field::new(..)
  .required() / .when(..) / .group(..)` and `Kind::one_of(Choices::fixed(..),
  custom, optional)` (botconf `kind.rs:97`). `derive` (67-115) builds
  `RoutingConfig::new(rules, connectors).with_notice_targets(..)` and validates
  it; `LiveRouting` (`routing.rs`) hands the core the current snapshot's
  `RoutingConfig` on every operation, so a setting carried in it applies live
  (plan 037). Configuration struct: `config.rs:13-46`. Panel drafts for forms
  with required fields: plan 044. Reference domains: `domains.rs` (`CHAT` =
  `wuphf.chat`, `connector:chat#thread`).
- **SDKs.** Go `coreclient` (`connectors/shared/coreclient/client.go`):
  `Sender` 80-91, optional capability interfaces (`ThreadRetitler` 97, `Reactor`
  106, …), the frame switch in `runSession` 253-332 (the `MessageRetitle` case
  277-288 is the pattern), `ackOnly` 137, `unsupportedOp` 143. Rust
  `crates/wuphf-connector/src/sender.rs`: trait methods with defaults
  (`apply_reaction` returns the target id); `client.rs` `handle_frame` matches
  each body. The core's acknowledgement mapping requires a non-empty
  `platform_message_id` (`grpc.rs`, `required_string`), so a chat-state ack
  returns the target chat id.
- **Session-loop hazard (045).** Senders must never block on the connector's
  input channel. Executing a chat-state dispatch emits nothing: the platform's
  own echo comes back through the normal event path.
- **whatsmeow** (`…/whatsmeow@v0.0.0-20260525123251-933deb5f2ee9`):
  `types/events/appstate.go` `Pin` (44) `{JID, Timestamp, Action
  *waSyncAction.PinAction, FromFullSync}`, `Mute` (77) with `MuteAction`,
  `Archive` (86) with `ArchiveChatAction`. `proto/waSyncAction`: `PinAction.Pinned`
  (7392), `MuteAction{Muted, MuteEndTimestamp (ms)}` (7436),
  `ArchiveChatAction{Archived, MessageRange}` (7012). `appstate/encode.go`:
  `BuildMute(target, mute, duration)` (43), `BuildMuteAbs(target, mute,
  *int64 endMs)` (52; nil + mute → -1 = forever), `BuildPin(target, pin)` (84),
  `BuildArchive(target, archive, time.Time{}, nil)` (98; zero values allowed,
  archive also unpins). `appstate.go:509` `(*Client).SendAppState(ctx,
  appstate.PatchInfo) error`. History sync `Conversation{Archived *bool (728),
  Pinned *uint32 (736), MuteEndTime *uint64 (737)}` in
  `proto/waHistorySync/WAWebProtobufsHistorySync.pb.go`.
- **whatsapp-rust** (`…/whatsapp-rust-6e26428647c827f3/5ca80c5`):
  `wacore/src/types/events.rs` `PinUpdate` (1414), `MuteUpdate` (1424; action
  `mute_end_timestamp: Option<i64>` ms, -1 forever), `ArchiveUpdate` (1434),
  each `{jid, timestamp, action, from_full_sync}`. `src/features/chat_actions.rs`
  via `client.chat_actions()` (769): `pin_chat` (428), `unpin_chat` (433),
  `mute_chat` (438, forever), `mute_chat_until(jid, end_ms)` (444; refuses a
  past or non-positive end), `unmute_chat` (465), `archive_chat(jid, None)`
  (410), `unarchive_chat(jid, None)` (419). History sync `Conversation {
  archived (whatsapp.proto:1912), pinned uint32 (1920), mute_end_time uint64
  (1921) }`.
- **Telegram connector** (`connectors/telegram/internal/telegram/`, gotd
  v0.144.0; forum methods are `messages.*`, not `channels.*`):
  `Normalizer.Dispatcher` (`updates.go:48-71`) handles new / edited messages and
  channel deletes only; `normalize` drops anything that is not `*tg.Message`
  (`updates.go:158-161`), so service messages are dropped. `formatPeer`
  (`targets.go:134`) builds chat ids; `PeerCache.InputPeer(ctx, chatID)`
  (`targets.go:108`) resolves them. `Sender` (`sender.go:29-47`, `bot` flag);
  `RetitleThread` (270-289) is the pattern for a topic operation
  (`tg.MessagesEditForumTopicRequest{Peer, TopicID}` + setters). The session is
  a bot when a bot token is configured, else a user (`connector.go:173-200`);
  all topic operations use the main client (`connector.go:130`).
  gotd `tg` (`…/gotd/td@v0.144.0/tg/`):
  `MessagesUpdatePinnedForumTopicRequest{Peer, TopicID, Pinned bool}`
  (`tl_messages_update_pinned_forum_topic_gen.go:37`);
  `MessagesEditForumTopicRequest` with `SetClosed(bool)`
  (`tl_messages_edit_forum_topic_gen.go:370`);
  `AccountUpdateNotifySettingsRequest{Peer InputNotifyPeerClass, Settings
  InputPeerNotifySettings}` (`tl_account_update_notify_settings_gen.go:38`)
  with `InputNotifyForumTopic{Peer, TopMsgID}` (`tl_input_notify_peer_gen.go:491`)
  and `InputPeerNotifySettings.SetMuteUntil` (`tl_input_peer_notify_settings_gen.go:415`).
  Updates: `UpdatePinnedForumTopic{Pinned, Peer, TopicID}` (`tl_update_gen.go:29959`,
  dispatcher `OnPinnedForumTopic`), `UpdatePinnedForumTopics{Peer, Order}`
  (30178, `OnPinnedForumTopics`), `UpdateNotifySettings{Peer NotifyPeerClass,
  NotifySettings PeerNotifySettings}` (3173, `OnNotifySettings`) with
  `NotifyForumTopic{Peer, TopMsgID}` (`tl_notify_peer_gen.go:488`); a topic
  closed or reopened arrives as a `*tg.MessageService` in
  `UpdateNewChannelMessage` whose `Action` is `*tg.MessageActionTopicEdit`
  with `GetClosed()` (`tl_message_action_gen.go:6205`). No gotd constant for
  "muted forever".

## Contract additions

`common.proto`:

```proto
// A chat's mute: 0 = not muted, -1 = muted forever, otherwise the end in Unix
// seconds. A mute whose end has passed is reported as 0.
message MuteState {
  int64 muted_until = 1;
}
```

`client.proto`, `ChatUpdate` gains:

```proto
  // Set when the update is about a thread (a Telegram forum topic) rather
  // than the whole chat. The core resolves (chat_id, thread_id) to the
  // conversation's mirror endpoint.
  optional string thread_id = 8;
  optional bool pinned = 9;
  optional MuteState mute = 10;
  // WhatsApp: archived. Telegram topic: closed.
  optional bool archived = 11;
```

`server.proto`:

```proto
// Bring a chat (or a thread) to this pin / mute / archive state. Absent fields
// are left alone. Acknowledged with the target chat id; a connector that
// cannot apply a field acknowledges without applying it.
message ChatStateDispatch {
  string dispatch_id = 1;
  string target_chat_id = 2;
  optional string target_thread_id = 3;
  optional bool pinned = 4;
  optional MuteState mute = 5;
  optional bool archived = 6;
}
```

`ServerFrame` gains `ChatStateDispatch chat_state = 9;`.

## Commands you will need

As in 045 / 046 (cargo per crate, Go one module at a time, whatsapp-rust in
Step 7 only), plus `nice -n 19 cargo test -p wuphf-settings` and the Telegram
module's `go vet` / `go test`.

## Scope

In: the protos and `proto/go`; `crates/wuphf-model` (`chat.rs`,
`dispatch.rs`, `connector.rs`); `crates/wuphf-storage` (`shared.rs`,
`tests.rs`, `migration_tests.rs`); `crates/wuphf-core` (new `chat_state.rs`,
`config.rs`, `profile.rs`, `lib.rs`, `tests.rs`); `crates/wuphf-settings`
(`config.rs`, `schema.rs`, `tests.rs`); `crates/wuphf-runtime` (`grpc.rs`,
`tests.rs`); `crates/wuphf-connector` (`sender.rs`, `client.rs`, tests);
`connectors/shared/coreclient`; `connectors/telegram/internal/telegram/*`;
`connectors/whatsapp/internal/whatsapp/*`; `connectors/whatsapp-rust/src/*`;
`config/example.toml`; docs; `plans/README.md`.

Out: Telegram dialog pins, folders and peer mutes for plain chats;
`MarkChatAsRead`, labels, other app-state actions; initial alignment of
existing topics; local config and secret files.

## Git workflow

Commit directly on `v2`, one commit per step. Do not push. No branches.

## Steps

### Step 0: Baseline

Drift check. Record `cargo test --workspace` and the Go module test results
(`shared`, `whatsapp`, `telegram`), one at a time. Do not build whatsapp-rust.

### Step 1: The contract

Apply "Contract additions". Neutral values at every Rust prost literal
(`thread_id: None, pinned: None, mute: None, archived: None`); in
`crates/wuphf-connector/src/client.rs` `handle_frame`, a temporary
`ChatState` arm that acknowledges with the target chat id (Step 4 replaces
it); in `crates/wuphf-runtime/src/grpc.rs` nothing yet (the core produces no
such dispatch until Step 3). Regenerate the Go bindings; add a `ChatState`
case to the Go `coreclient` frame switch that acknowledges unchanged (Step 4
adds the interface).

**Verify**: `git status --short proto/go` → only `common.pb.go`,
`client.pb.go`, `server.pb.go`; `cargo check -p wuphf-proto -p
wuphf-connector -p wuphf-runtime --all-targets` → exit 0; `proto/go`,
`connectors/shared` and `connectors/telegram` `go vet ./...` → exit 0.
Commit: `feat(proto): chat state updates and dispatches`.

### Step 2: Model and settings

Model (stored types keep `skip_serializing_if` on every new field, so 045 /
046 rows keep their exact JSON):

- `chat.rs`: `ChatProfile` gains `pinned: Option<bool>`, `mute:
  Option<MuteState>`, `archived: Option<bool>`; `pub struct MuteState {
  pub muted_until: i64 }` with `const FOREVER: i64 = -1`; `ChatUpdate` gains
  `thread_id: Option<String>`, `pinned`, `mute`, `archived`.
  `ChatStateDirection { Off, ToMirror, ToSource, Both }` (serde
  `snake_case`: `off`, `to_mirror`, `to_source`, `both`) with
  `fn allows(self, from_source: bool) -> bool`. `ChatStatePolicy { pin, mute,
  archive: ChatStateDirection }` (default all `Both`).
- `dispatch.rs`: `Action::SetChatState { pinned: Option<bool>, mute:
  Option<MuteState>, archived: Option<bool> }`; no message id, records no
  copy. `shared.rs` `bind_native_parent`: no-op arm.

Settings (`crates/wuphf-settings`), defined like `rules` and `notify`:

- `config.rs`: `Configuration.chat_state: ChatStateConfig` (`#[serde(default)]`)
  with `pin`, `mute`, `archive: ChatStateDirection` (each defaulting to
  `both`) and `chats: BTreeMap<String, ChatStateOverride>`;
  `ChatStateOverride { chat: ChatTarget, pin, mute, archive:
  Option<ChatStateDirection> }`.
- `schema.rs` `settings()`: three `RuntimeSetting::new("chat_state.pin" | …)
  .titled(..).kind(Kind::one_of(Choices::fixed(DIRECTIONS), false, false))`
  with `DIRECTIONS = [off "Off", to_mirror "Source → mirror", to_source
  "Mirror → source", both "Both ways"]` (generic, since a source need not be
  WhatsApp), and
  `RuntimeSetting::per_entry("chat_state.chats", "Per-conversation overrides
  of chat_state; the chat is the source chat or its mirror topic",
  CHAT_STATE_OVERRIDE)` whose form has `chat` (`Kind::reference(domains::CHAT,
  false)`, `.required()`) and `pin` / `mute` / `archive` (`Kind::one_of(..,
  false, true)`, optional). A new entry is built in a panel draft (044)
  because `chat` is required.
- `derive`: validate each override's `chat` names a configured connector,
  build `ChatStateSettings { global: ChatStatePolicy, overrides:
  Vec<(ChatRef, ChatStatePolicyOverride)> }` and pass it as
  `RoutingConfig::new(..).with_notice_targets(..).with_chat_state(..)`.
- `crates/wuphf-core/src/config.rs`: `RoutingConfig.chat_state` with
  `with_chat_state(..)` and `fn chat_state_policy(&self, conversation:
  &Conversation, endpoints: &[ChatRef]) -> ChatStatePolicy`: the first
  override whose `chat` equals the conversation's source or one of its
  endpoints' addresses, field by field over the global value.
- `config/example.toml`: a commented `[chat_state]` section with the three
  keys and one `[chat_state.chats.<name>]` example.

Tests:
1. `crates/wuphf-settings/src/tests.rs`: `chat_state_defaults_to_both`;
   `chat_state_is_live` (set `chat_state.mute = "off"` on the store →
   `LiveRouting` sees it); `a_chat_state_override_is_created_through_a_draft`
   (panel draft: add entry → `chat` required → Save, like 044's rule test);
   `an_override_naming_an_unknown_connector_is_refused`.
2. `crates/wuphf-core` config tests: `an_override_matches_the_source_or_the_mirror_topic`
   and falls back field by field.
3. Storage: `chat_profiles_round_trip_every_field` (046) extended with the
   three states; a 046-shaped profile decodes with them `None`
   (`migration_tests.rs`). No migration: the fields live in the JSON payload.

**Verify**: `nice -n 19 cargo test -p wuphf-model -p wuphf-storage -p
wuphf-settings -p wuphf-core` → green. If botconf refuses `chat_state.chats`
as a per-entry key (a dotted key for a map), name it `chat_state_chats`
instead, keep everything else, and record the deviation.
Commit: `feat(settings): chat_state directions with per-conversation overrides`.

### Step 3: The core syncs chat state

New `crates/wuphf-core/src/chat_state.rs`, called from `update_chats` for any
update carrying `pinned`, `mute` or `archived`:

```rust
/// Apply one side's report of a conversation's chat state and return the
/// dispatches that carry it to the other side.
///
/// The state lives in the source chat's profile. A report equal to it is a
/// no-op — every echo of a dispatch this function sent ends here. A report
/// from the source side is always stored (the profile mirrors the source);
/// it is dispatched to each mirror endpoint with a thread when the policy's
/// direction allows `to_mirror` and the update is not a snapshot. A report
/// from the mirror side is stored and dispatched to the source endpoint only
/// when the direction allows `to_source`; otherwise it is dropped.
pub(crate) async fn sync_chat_state(…) -> Result<Vec<Dispatch>, CoreError>;
```

- **Side.** Resolve the update: with `thread_id`, `resolve_endpoint(connector,
  ChatAddress(chat, thread))` → that endpoint's conversation; the report is
  from the mirror side if `Router::is_mirror(conversation, endpoint)`. Without
  `thread_id`, 046's `conversations_of_chat` (the source side). An update for
  a chat in no conversation is stored on its own profile (WhatsApp side) or
  dropped (a thread nobody maps).
- **Per field.** Compare with the source profile; build one
  `Action::SetChatState` per target carrying only the fields that changed and
  are allowed; skip a target that would get no fields.
- **Targets.** Source side → each mirror endpoint that has a thread (none
  without one: nothing to pin yet). Mirror side → the endpoint the
  conversation's source resolves to.
- **Snapshots** (046's flag) store source-side values and dispatch nothing;
  a mirror-side snapshot does not exist.
- The policy comes from `routing.chat_state_policy(..)` of the operation's
  snapshot.

Tests (`crates/wuphf-core/src/tests.rs`, WhatsApp group + Telegram mirror with
a provisioned thread):

1. `a_whatsapp_pin_pins_the_topic` → one `SetChatState { pinned: Some(true) }`
   to the Telegram endpoint with its thread.
2. `the_topic_pin_echo_is_a_no_op` — then Telegram reports the topic pinned →
   no dispatch, nothing written.
3. `a_topic_close_archives_the_whatsapp_chat` → `SetChatState { archived:
   Some(true) }` to the WhatsApp endpoint; WhatsApp's echo → no-op.
4. `mute_values_cross_exactly` — WhatsApp `muted_until = 1_900_000_000` →
   the dispatch carries that value; Telegram echoing it → no-op; forever (-1)
   both ways.
5. `directions_gate_each_field` — `pin = to_source`: a WhatsApp pin is stored
   but not dispatched; a Telegram pin is stored and dispatched; `mute = off`:
   neither side's mute moves; `to_mirror`: a Telegram archive is dropped and
   the stored value stays.
6. `an_override_beats_the_global_setting` for one conversation only.
7. `a_snapshot_sets_the_baseline_silently` — history-sync state → stored, no
   dispatch; then a different live value → dispatched.
8. `no_topic_no_dispatch` — a mirror without a thread → nothing.
9. `the_archive_unpin_crosses_too` — WhatsApp reports archived, then unpinned
   (as `BuildArchive` does) → two dispatches, the topic closed and unpinned.

**Verify**: `nice -n 19 cargo test -p wuphf-core` → green. Mutations: compare
against the mirror's own report instead of the source profile → test 2
fails; skip storing a source-side report when the direction forbids
`to_mirror` → test 5 fails (the later Telegram pin is wrongly dispatched).
Commit: `feat(core): two-way pin, mute and archive through the source chat's profile`.

### Step 4: Wire mapping and SDKs

- `crates/wuphf-runtime/src/grpc.rs`: the `ChatUpdate` fields (`MuteState`,
  `thread_id`); `Action::SetChatState` → `ServerFrame::ChatState`.
- Rust SDK (`crates/wuphf-connector`): `Sender::apply_chat_state(&self,
  pb::ChatStateDispatch) -> Result<String, Failure>` with a default that
  returns `target_chat_id` (acknowledge unchanged); `handle_frame` delivers it
  like the others.
- Go `coreclient`: `ChatStateApplier` interface (`ApplyChatState(ctx,
  *wuphfpb.ChatStateDispatch) (string, error)`), the case using it or
  `ackOnly` with the target chat id.

Tests: grpc mapping; Rust SDK session test — a sender without the method
acknowledges with the chat id; Go `client_test.go` — same, and an
implementing sender is called.

**Verify**: `cargo test -p wuphf-runtime -p wuphf-connector`; `connectors/shared`
`go test ./...` → green.
Commit: `feat(runtime): chat state dispatches on the wire and in both SDKs`.

### Step 5: Telegram observes and applies topic state

`connectors/telegram/internal/telegram/`:

- **Observe** (`updates.go` `Dispatcher`, a new `chatstate.go` for the
  mapping): `OnPinnedForumTopic` → `ChatUpdate{chat_id: formatPeer(Peer),
  thread_id: strconv.Itoa(TopicID), pinned: Pinned}`; `OnPinnedForumTopics` →
  `pinned: true` for each topic in `Order`; `OnNotifySettings` with a
  `*tg.NotifyForumTopic` peer (user sessions only) → `mute` from
  `NotifySettings.GetMuteUntil()`: unset or not in the future → 0,
  `math.MaxInt32` → -1, else the value; `OnNewChannelMessage` → before
  `handle`, a `*tg.MessageService` whose `Action` is
  `*tg.MessageActionTopicEdit` with `GetClosed()` set → `archived: closed`
  for the topic it belongs to (the service message's reply header names the
  topic: reuse `topicRootID`'s logic for `*tg.MessageService`). These frames
  are sent from update handlers, which may block on the input channel.
- **Apply** (`sender.go`, `ApplyChatState`): resolve the peer with
  `PeerCache.InputPeer`; a dispatch without `target_thread_id` → acknowledge
  without acting (plain chats are out of scope); `pinned` →
  `MessagesUpdatePinnedForumTopic{Peer, TopicID, Pinned}`; `archived` →
  `MessagesEditForumTopic` with `SetClosed`; `mute` (user session only; a bot
  logs once and skips) → `AccountUpdateNotifySettings{Peer:
  &tg.InputNotifyForumTopic{Peer, TopMsgID: topicID}, Settings: (with
  SetMuteUntil(-1 → math.MaxInt32, else the value))}`. A `TOPIC_NOT_MODIFIED`
  style error (already in that state) counts as success. Return the target
  chat id.

Tests: the update → `ChatUpdate` mappings (pin, bulk pin, notify with each
mute shape, topic close / reopen, a non-topic notify ignored, bot mode
ignores mutes); `ApplyChatState` against a recording fake API (the requests
and their fields; forever → `MaxInt32`; no thread → no request; bot → no
notify request).

**Verify**: `connectors/telegram` `go vet ./...` and `go test ./...` → green.
Commit: `feat(telegram): sync forum topic pin, mute and close`.

### Step 6: Go WhatsApp

- **Observe** (`connector.go` switch → `updates.go`): `*events.Pin` →
  `pinned: Action.GetPinned()`; `*events.Mute` → `mute`: not muted → 0,
  `MuteEndTimestamp < 0` → -1, else `ms / 1000`, and 0 when that is not in the
  future; `*events.Archive` → `archived`. Chat id through `n.canonical`.
  `FromFullSync` → a snapshot frame. History sync (045 / 046's
  `HandleHistorySync`, already a snapshot) adds `pinned = GetPinned() > 0`,
  `archived = GetArchived()`, and `mute` from `GetMuteEndTime()` — treat
  values above 10^11 as milliseconds and smaller ones as seconds (the unit is
  undocumented and whatsmeow never reads it), 0 → not muted.
- **Apply** (`sender.go` `ApplyChatState`; `SendClient` gains
  `SendAppState(ctx, appstate.PatchInfo) error`): `pinned` →
  `appstate.BuildPin(chat, pinned)`; `mute` → `BuildMuteAbs(chat, until != 0,
  endMs)` with `endMs = nil` for forever and `until*1000` otherwise;
  `archived` → `BuildArchive(chat, archived, time.Time{}, nil)`. One
  `SendAppState` per field. The chat is the dispatch's target in its
  addressing form (`s.target`). Return the target chat id.

Tests: event mappings (each shape, `FromFullSync` → snapshot, expired mute →
0); history-sync mapping (ms vs s); `ApplyChatState` with a recording fake
(`BuildMuteAbs` gets exactly `until*1000`; forever → nil; one patch per
field).

**Verify**: `connectors/whatsapp` `go vet ./...` and `go test ./...` → green.
Commit: `feat(whatsapp): sync pin, mute and archive through app state`.

### Step 7: whatsapp-rust

Build once at the end (`cargo test`, then `cargo clippy`), alone.

- **Observe** (`chats.rs`; register `EventKind::PinUpdate`, `MuteUpdate`,
  `ArchiveUpdate` with 045's `on_event_for`): the same mappings as Step 6
  (`mute_end_timestamp` ms, -1 forever, expired → 0), `from_full_sync` →
  snapshot; history sync adds `pinned`, `archived`, `mute_end_time` with
  Step 6's unit rule. DM chats reported under both forms (045's `forms`).
- **Apply** (`sender.rs` `apply_chat_state`): `client.chat_actions()`:
  `pin_chat` / `unpin_chat`; `mute_chat` for -1, `unmute_chat` for 0,
  `mute_chat_until(jid, until*1000)` otherwise (an end already past →
  `unmute_chat`); `archive_chat(jid, None)` / `unarchive_chat(jid, None)`.
  Return the target chat id.

Tests: pure mapping functions for the three events and the history-sync
fields; a pure `fn mute_call(until: i64, now_ms: i64) -> MuteCall` deciding
which chat action to use.

**Verify**: `(cd connectors/whatsapp-rust && RUSTC_BOOTSTRAP=1
CARGO_BUILD_JOBS=2 nice -n 19 cargo test)` → green; clippy clean; lock
unchanged.
Commit: `feat(whatsapp-rust): sync pin, mute and archive`.

### Step 8: Docs

- `docs/connector-protocol.md`: `ChatUpdate.thread_id`, `pinned`, `mute`,
  `archived` (mute encoding, expired = 0, Telegram close = archived);
  `ChatStateDispatch` (absent fields untouched; acknowledge with the chat id;
  unsupported fields acknowledged without acting).
- `docs/architecture.md`: "Chat state" under Chat Profiles — one state per
  conversation in the source profile, echo = equal value, directions,
  snapshots, the Telegram mapping and the bot-mode mute limit.
- `README.md`: one bullet; the settings section lists `chat_state`.

Commit: `docs: two-way chat state`.

### Step 9: Gate and index

`cargo test --workspace`, clippy, fmt; Go modules one at a time
(`proto/go`, `shared`, `whatsapp`, `telegram`, `instagram` build);
`plans/README.md` row 047 DONE with notes. Commit: `docs(plans): mark plan 047 done`.

## Test plan

| Test | Where | Proves |
|---|---|---|
| settings defaults / live / draft / override validation | wuphf-settings | decision 2 |
| override matching | wuphf-core config | per-conversation overrides |
| pin, echo, close, exact mute, directions, override, snapshot, no topic, archive-unpin | wuphf-core | decisions 1, 3, 4, 6 |
| grpc + SDK defaults | runtime, connector, coreclient | the wire, ack fallback |
| observe / apply | telegram, Go whatsapp, whatsapp-rust | decisions 3, 5, 6 |

## Done criteria

- [ ] Every test passes; each listed mutation fails a test.
- [ ] No migration; 046-shaped profiles decode.
- [ ] whatsapp-rust built once; lock unchanged.
- [ ] Docs updated; row 047 DONE.

## STOP conditions

- The drift check fails, or 045 / 046 are not DONE.
- Regenerating the Go bindings changes other files than the three protos'.
- A library fact is missing: whatsmeow `BuildMuteAbs` / `BuildPin` /
  `BuildArchive` / `SendAppState` / `events.Pin|Mute|Archive`; whatsapp-rust
  `chat_actions()` methods or `PinUpdate|MuteUpdate|ArchiveUpdate`; gotd
  `MessagesUpdatePinnedForumTopicRequest`, `OnPinnedForumTopic`,
  `OnNotifySettings`, `InputNotifyForumTopic`, `MessageActionTopicEdit`.
- botconf refuses both `chat_state.chats` and `chat_state_chats` as a
  per-entry setting.
- A live test (if the owner runs one) shows the two sides changing a state
  back and forth: the echo comparison is wrong; do not add a delay.

## Maintenance notes

- **Never emit frames from a `Sender`** (045's hazard). Applying a state
  emits nothing; the platform's echo returns through the event handlers, which
  may block on the input channel.
- **An echo is a no-op only while both sides encode the value identically.**
  Any new state field needs an exact, reversible mapping (as mute's ms ↔ s
  and forever ↔ `MaxInt32`), or the sides will fight.
- **History sync mute unit**: the 10^11 threshold separates seconds from
  milliseconds for any date before the year 5138; replace it once the unit is
  confirmed from a real sync.
- Telegram plain chats (one-to-one rules) could map to dialog pins
  (`messages.toggleDialogPin`), folders (archive) and peer notify settings in
  a later plan; the contract already allows a dispatch without a thread.
