# Plan 046: Chat profiles — titles, descriptions, avatars, participants, and notes in mirror threads

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
> in Step 6 (one `cargo test`, then one `cargo clippy`), never alongside
> another build. No release builds, no `nix flake check`, no dev-bin installs.
> Only one agent builds at a time.
>
> **Drift check (run first)**: plan 045 must have landed. Find its last
> commit with `git log --oneline --grep='mark plan 045 done' -1` (call it
> `$P045`), then run
> `git diff --stat $P045..HEAD -- crates migrations proto/go connectors/shared/coreclient connectors/whatsapp connectors/whatsapp-rust connectors/telegram docs`.
> Expected: empty. What must still hold (if any is false, STOP):
> - `plans/README.md` row 045 is DONE. HEAD at planning time was `4dd923a`
>   (045 not yet executed); everything below is 045's expected output.
> - `migrations/0019_chat_profiles.sql` is the newest migration and creates
>   `chat_profiles (connector_id, chat_id, payload)`.
> - `crates/wuphf-proto/client.proto`: `ChatUpdate { chat_id = 1; optional
>   DisappearingTimer disappearing = 2; }`, `ChatUpdateEvent { repeated
>   ChatUpdate updates = 1; }`, `ClientFrame.chat_update = 11`,
>   `NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN = 5`. `server.proto`:
>   `MessageCreateDispatch.disappearing = 13` is its last field. `common.proto`:
>   `DisappearingTimer`, `Capabilities.supports_disappearing_timers = 3`.
> - `crates/wuphf-model/src/chat.rs` defines `DisappearingTimer`, `ChatProfile
>   { connector_id, chat_id, disappearing }`, `ChatUpdate { chat_id,
>   disappearing }`; `InboundWork::ChatUpdate { connector_id, updates }`;
>   `Action::Send { message, reply, disappearing }`.
> - `crates/wuphf-storage/src/lib.rs`: `ChatProfileStore { chat_profile,
>   put_chat_profile }` is part of `Storage`.
> - `crates/wuphf-core/src/profile.rs`: `merge_disappearing`, `apply`,
>   `Core::update_chats(&ConnectorId, Vec<ChatUpdate>) -> Result<usize, _>`,
>   `target_disappearing`, `address_forms`; `Core::observe_message_timer`
>   (the unknown fill).
> - whatsapp-rust: `src/{groups.rs,names.rs,chats.rs,disappearing.rs}` exist;
>   `names.rs` has `tagged`, `format_person_name`, `dm_title`; `normalize` is
>   `normalize(info, message, chat_title)`. Go whatsapp:
>   `internal/whatsapp/disappearing.go` exists; `HandleGroupInfo`,
>   `HandleHistorySync`, `sendChatUpdates` in `updates.go`.
> - Library pins unchanged: whatsmeow `v0.0.0-20260525123251-933deb5f2ee9`
>   (`connectors/whatsapp/go.mod`), whatsapp-rust `rev = "5ca80c5c"`
>   (`connectors/whatsapp-rust/Cargo.toml`), gotd `v0.144.0`
>   (`connectors/telegram/go.mod`).

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED — new messages appear in mirror threads (notes, info cards);
  the provisioning path changes; a new table in the Rust connector's own
  database
- **Depends on**: 045
- **Category**: feature
- **Planned at**: commit `4dd923a`, 2026-10-31 (045 planned, not executed)

## Why this matters

A mirror topic shows the messages of a WhatsApp chat, but nothing about the
chat itself: who is in the group, what it is about, what its picture is, when
someone joins or leaves, when the disappearing timer changes. The owner reading
WhatsApp through Telegram misses all of it. The Rust connector also still
names a DM the owner starts with its raw id (045's caveat), and shows group
senders by push name where the Go connector shows the saved contact name.

This is slice 2 of 3. It extends 045's chat profiles (`chat_profiles` payload,
`ChatUpdate`, `ChatUpdateEvent`) without reshaping them. Slice 3 (047) adds
pin / mute / archive.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | A chat profile holds title, description, avatar, participant count and the disappearing timer (045). The core stores it behind `Storage`, in 045's `chat_profiles` JSON payload. A title change goes through the existing retitle path; every other change becomes a **note** in each mirror thread. |
| 2 | The first time a value is seen (a history-sync warm-up, a newly seen chat) it is stored silently as the baseline. Only later real changes produce notes. |
| 3 | Notes are system messages. They travel as a `system_note` flag on the existing `MessageCreateDispatch`, so media (the avatar photo) reuses the existing path. The core records no message mapping for them, so a reply to a note links to nothing. They are never bridged back to the source or to any other platform. They carry no sender label and no reply. |
| 4 | Info card: when the core provisions a mirror topic it posts one info card (title, description, avatar, timer, participant count) before the first message. After that, only change notes. |
| 5 | Avatar: a change posts a note with the photo attached. The core stores a hash so the same picture is never posted twice, and keeps the avatar for the TUI and the panel. Telegram topics cannot have photos. |
| 6 | Participants: join, leave, promote and demote notes, naming the actor when the library reports one ("Alice added Bob", "Alice removed Bob", "Alice made Bob an admin", "Alice dismissed Bob as admin"), and the plain wording ("Bob joined", "Bob left") when there is no actor or the actor is the member themself (owner, 2026-10-31). Group senders resolve to contact names instead of push names. @mentions render as names. The core stores only the count, never the member list. |
| 7 | Timer changes produce a note ("Disappearing messages: 7 days" / "off"). |
| 8 | Name sources: Go keeps whatsmeow `Store.Contacts`. Rust gets its own small contacts table in the connector's SQLite file (the file whatsapp-rust's session store uses), filled from history-sync push names, `Event::ContactUpdate` (full / first name) and each message's `MessageInfo.push_name`. Not whatsapp-rust's chat-store crate: it stores the whole message history. Rust DM titles follow Go's `formatPersonName` order. A chat the owner starts gets a real name from the table. |
| 9 | Change sources: whatsmeow `events.GroupInfo` (Name, Topic, Join / Leave / Promote / Demote, Ephemeral), `events.Picture`, `events.Contact`, `events.PushName`, `events.BusinessName`, history sync. whatsapp-rust `Event::GroupUpdate` actions, `Event::PictureUpdate`, `Event::ContactUpdate`, history sync. Each change reaches the core as a `ChatUpdateEvent` with partial fields. |
| 10 | Out of scope: mute / archive / pin (047), mirroring the timer as deletes, anything two-way. |

### Further decisions (accepted by owner 2026-10-31 — do not revisit)

- **`ChatUpdateEvent.snapshot`.** History sync and the first-sighting report
  (below) are snapshots of current state, not live changes. The connector says
  so (`bool snapshot = 2`), and the core stores a snapshot without notes even
  when it differs from what it knew. Without this, re-pairing would post a
  note for every chat whose description or member count drifted while the
  bridge was down. It is a fact about the report, not a bridge decision.
- **First sighting.** The first time a connector process sees a chat (an
  inbound message; never a dispatch, since a `Sender` must not emit frames),
  it sends one snapshot update with
  the chat's title, description, participant count and avatar, before the
  message. That is what gives a new chat's info card its content. The cost is
  one group-info call (already made for the title) and one profile-picture
  call per active chat per process start.
- **Avatar upload.** The connector downloads the picture and uploads it to the
  core's media store (`MediaTransport.UploadMedia`), and sends the picture id
  and media id. The core hashes the bytes (SHA-256) and compares with the
  stored hash; an equal hash updates nothing. Repeated uploads of the same
  picture become orphans, which the existing 24-hour orphan pruning removes.
  Pruning must spare the media a profile still references (Step 2).
- **The info card provisions the topic.** When routing would provision a
  mirror thread for a conversation that has never had a card, the core sends
  the card as the provisioning dispatch (a note with `create_thread`) and puts
  the message in the existing durable provisioning stash; the stash replays
  it once the card's acknowledgement establishes the thread. A
  `Conversation.introduced` flag, set when the card is created, guarantees one
  card per conversation even if the card fails (a failed card is dead-lettered,
  the stash replays, and the message provisions the thread itself).
- **The title in a `ChatUpdate` is the display title**, exactly the string the
  connector sends as `MessageCreateEvent.chat_title` (tagged `[G] …` /
  `[P] …`). A plain group name would differ from the message-carried title and
  rename the topic back and forth.
- **Notes go to every other endpoint of each conversation the chat belongs to**,
  skipping a mirror endpoint whose thread does not exist yet (its info card
  will show the current state).
- **The Rust contacts table is `wuphf_contacts`**, not `contacts`:
  whatsapp-rust's chat-store crate creates a `contacts` table in the same file,
  and a later switch to it must not collide. It is reached through the session
  store's shared pool (Step 6 explains why).
- **Rust has no business-name source.** whatsapp-rust has no business-name
  event, and `MessageInfo.verified_name` is a signed certificate, not a plain
  name. The Rust title order is saved full name, then push name, then number;
  Go keeps its business-name step.
- **Membership wording.** Each change carries an optional actor (canonical id
  plus display name, resolved by the connector like any sender name). The
  core picks the wording: with an actor that is not the member, "➕ Alice
  added Bob", "➖ Alice removed Bob", "⭐ Alice made Bob an admin", "Alice
  dismissed Bob as admin"; without one, or when the actor id equals the
  member id, "➕ Bob joined", "➖ Bob left", "⭐ Bob is now an admin", "Bob is
  no longer an admin". The connector reports the actor in the same address
  form as the members, so the self check is a plain id comparison.

## Current state (after 045)

- **Contract** as listed in the drift check. Go bindings are generated by hand
  (045's Step 1 command, inside `nix develop`):
  ```sh
  cd crates/wuphf-proto && protoc \
    --go_out=../../proto/go --go_opt=module=github.com/wuphf-rs/wuphf-ai/proto/go \
    --go-grpc_out=../../proto/go --go-grpc_opt=module=github.com/wuphf-rs/wuphf-ai/proto/go \
    common.proto media.proto client.proto server.proto connector.proto functions.proto
  ```
- **Provisioning.** `Router::route` gives a mirror target with no thread a
  `Target.create_thread` (`crates/wuphf-core/src/router.rs:304-317`,
  `resolve_target`). `Core::route_and_record` (`ingest.rs:232-258`) saves the
  dispatches and registers those with `create_thread` in `self.provisioning`.
  Messages arriving meanwhile go to the durable stash (`stash_deferred`,
  `acknowledge.rs:146-170`); `resume_provisioned` / `replay_stash`
  (`acknowledge.rs:97-140`) replay them through `route_and_record` once every
  provisioning dispatch has settled (acknowledged or failed).
  `persist_provisioned_thread` (`acknowledge.rs:212-230`) stamps the thread on
  the endpoint. `delivery_ack_for` (`acknowledge.rs:63-91`) only fires for
  `Action::Send`.
- **Retitle.** `maybe_retitle` (`ingest.rs:149-184`) updates
  `Conversation.title` and calls `Router::route_retitle` (`router.rs:244-270`),
  which renames only provisioned mirror threads.
- **Copies.** `Dispatch::records_copy` / `message_id`
  (`crates/wuphf-model/src/dispatch.rs:40-61`) decide whether an
  acknowledgement records a copy; `shared.rs:10-19` applies it for both
  backends. `bind_native_parent` (`shared.rs:30-53`) matches every `Action`
  variant.
- **Conversation** (`crates/wuphf-model/src/routing.rs:422-436`): `id,
  rule_id, source, endpoints, title`. Narrow writers in `TopologyStore`
  (`set_conversation_title`, `add_conversation_endpoint`).
- **Media pruning.** `prune_unreferenced_media_before`
  (`crates/wuphf-storage/src/sqlite.rs:413-426`) deletes blobs older than the
  cutoff with no `message_media` row; retention is
  `orphan_media_retention_seconds`, default 24 hours
  (`crates/wuphf-settings/src/config.rs:52-54`). The in-memory backend mirrors
  it in `memory.rs`.
- **Rust SDK** (`crates/wuphf-connector`): a session client and the `Sender`
  trait only — **no media upload client**. The Go connectors upload through
  `MediaBroker.Upload(ctx, *MediaMetadata, []byte) (*Attachment, error)`.
- **Session-loop hazard (from 045).** Both SDK session loops run a dispatch and
  drain the connector's input channel in one loop
  (`crates/wuphf-connector/src/client.rs:164-181`,
  `connectors/shared/coreclient/client.go:242-…`). Code reached from a
  `Sender` must never block on the input channel; event handlers and
  normalizers may.
- **Telegram connector** (`connectors/telegram/internal/telegram/`):
  `Sender.SendMessage` (`sender.go:75-139`) creates the topic when
  `provision_thread` is set (`createTopic`, 364-381), replies to the topic root,
  and renders a bold "🗣️: name" header only when `participant` is set
  (`senderLabel` 297, `attributedText` 312). Its own outgoing messages are
  dropped before mirroring (`updates.go:168`), so the bridge's notes never come
  back as inbound messages.
- **Go WhatsApp connector**: `chatNamer` (`names.go`: `titleTTL` 16, `resolve`,
  `PersonName`, `formatPersonName` 129-144) reads `Store.Contacts`; whatsmeow
  stores history-sync push names itself (`message.go:762` →
  `handleHistoricalPushNames`, `user.go:639-651`). `senderName`
  (`updates.go:371-385`) already uses `PersonName` for group senders. The event
  switch is in `connector.go`.
- **whatsmeow facts** (module cache path as in 045):
  `types/events/events.go:461-496` `GroupInfo{JID, Sender, Timestamp, Name
  *types.GroupName (468), Topic *types.GroupTopic (469), Ephemeral (472), Join
  (488), Leave (489), Promote (491), Demote (492) []types.JID}`;
  `events.go:501` `Picture{JID, Author, Timestamp, Remove bool, PictureID}`;
  `types/events/appstate.go:18` `Contact{JID, Timestamp, Action
  *waSyncAction.ContactAction, FromFullSync}`, `:27` `PushName{JID, JIDAlt,
  Message, OldPushName, NewPushName}`, `:36` `BusinessName{JID, Message,
  OldBusinessName, NewBusinessName}`. `types/group.go:70` `GroupName{Name,
  …}`, `:78` `GroupTopic{Topic, TopicID, …, TopicDeleted bool}`, `:43`
  `GroupInfo.Participants []GroupParticipant`. `user.go:528`
  `GetProfilePictureInfo(ctx, jid, *GetProfilePictureParams)
  (*types.ProfilePictureInfo, error)`; `types/user.go:54`
  `ProfilePictureInfo{URL, ID, Type, DirectPath, Hash}` — `URL` is a plain
  HTTP GET. `store/store.go:102` `ContactStore.GetContact`;
  `types/user.go:65` `ContactInfo{Found, FirstName, FullName, PushName,
  BusinessName}`. `proto/waE2E` `ContextInfo.MentionedJID []string` (9008).
  `waHistorySync.Conversation.Name` (725), `DisplayName` (750).
- **whatsapp-rust facts** (checkout path as in 045):
  `wacore/src/stanza/groups.rs:114-…` `GroupNotificationAction`: `Add {
  participants: Vec<GroupParticipantInfo>, reason }` (118), `Remove { … }`
  (124; leaving arrives as `Remove`), `Promote { participants }` (130),
  `Demote { participants }` (135), `Subject { subject, subject_owner,
  subject_time }` (147), `Description { id, description: Option<String> }`
  (154), `Ephemeral` (178). `GroupParticipantInfo { jid, phone_number:
  Option<Jid>, display_name: Option<String> }` (77).
  `wacore/src/types/events.rs`: `PictureUpdate { jid, author, timestamp,
  removed: bool, picture_id: Option<String> }` (1291), `ContactUpdate { jid,
  timestamp, action: Box<ContactAction>, from_full_sync }` (1394; proto
  `ContactAction { full_name, first_name, lid_jid, pn_jid, … }`,
  `waproto/src/whatsapp.proto:5654`), `PushNameUpdate` (1404). History sync
  `pushnames` (`whatsapp.proto:2442`, `Pushname { id, pushname }` 5114) — and
  **whatsapp-rust does not store other people's push names**
  (`wacore/src/history_sync.rs:331` reads only its own).
  `src/features/contacts.rs:195` `client.contacts().get_profile_picture(&jid,
  preview) -> Result<Option<ProfilePicture>, _>`;
  `wacore/src/iq/contacts.rs:35` `ProfilePicture { id, url, direct_path, hash }`.
  Download: `client.http_client` (`src/client.rs:839`) with
  `HttpRequest::get(url)` (`wacore/src/net.rs:123`) → `HttpResponse {
  status_code, body }` (154). `src/features/groups.rs:95-…` `GroupMetadata {
  subject, participants, description, … }`. `MessageInfo.push_name`
  (`wacore/src/types/message.rs:304`); `verified_name` is a certificate (312).
  `ContextInfo.mentioned_jid: Vec<String>` (`whatsapp.proto:1629`).
- **whatsapp-rust storage.** `whatsapp-rust-sqlite-storage` 0.6.0 (diesel
  2.3.11 + r2d2, libsqlite3-sys 0.37.0 in `connectors/whatsapp-rust/Cargo.lock`;
  no rusqlite). `SqliteStore::shared() -> SharedSqlite`
  (`storages/sqlite-storage/src/shared.rs:55`), documented as the handle "for
  sibling crates to run their own queries and migrations against this store's
  database file through the same pool and semaphore";
  `SharedSqlite::run(FnOnce(&mut SqliteConnection) -> Result<T, StoreError>)`
  (28) runs on a blocking thread. The store runs its own embedded diesel
  migrations at startup (`sqlite_store.rs:52, 343`). `SharedSqlite` is not
  re-exported by `whatsapp_rust::store`. The chat-store crate
  (`storages/chat-store`, not in our lock) keeps chats, messages, reactions,
  receipts, media refs and a `contacts` table; it has no option to skip
  messages.

## Contract additions

`client.proto` (fields added to 045's messages; nothing renumbered):

```proto
message ChatUpdate {
  string chat_id = 1;
  optional DisappearingTimer disappearing = 2;
  // The chat's display title, exactly as the connector sends it in
  // MessageCreateEvent.chat_title (tags included). A change renames the
  // mirror thread; it never becomes a note.
  optional string title = 3;
  // The group description / chat about text. Empty means cleared.
  optional string description = 4;
  optional ChatAvatar avatar = 5;
  // The member count, when the connector read it (group info, a snapshot).
  optional uint32 participant_count = 6;
  // Membership events, in order. Never part of a snapshot.
  repeated ParticipantChange participant_changes = 7;
}

// The chat's picture. `id` is the platform's picture id; empty means the
// picture was removed. `media_id` is the uploaded picture in the core's media
// store, absent when removed or when the connector could not fetch it.
message ChatAvatar {
  string id = 1;
  optional string media_id = 2;
}

message ParticipantChange {
  enum Kind {
    KIND_UNSPECIFIED = 0;
    KIND_JOINED = 1;
    KIND_LEFT = 2;
    KIND_PROMOTED = 3;
    KIND_DEMOTED = 4;
  }
  Kind kind = 1;
  // Canonical participant id, as the connector reports senders.
  string participant = 2;
  // Display name as the connector resolves senders (contact name first).
  optional string name = 3;
  // Who made the change (the admin who added, removed, promoted or demoted
  // the member), when the platform says: canonical id in the same address
  // form as `participant`, and display name resolved like `name`. Absent for
  // a self-join through a link or a self-leave the platform reports without
  // an actor. The core uses the plain wording when it is absent or equals
  // `participant`.
  optional string actor = 4;
  optional string actor_name = 5;
}

message ChatUpdateEvent {
  repeated ChatUpdate updates = 1;
  // The updates describe current state (a history sync, a first sighting),
  // not live changes: the core stores them without notes.
  bool snapshot = 2;
}
```

`server.proto`, `MessageCreateDispatch` gains:

```proto
  // A note the core composed about the chat (an info card, a change). Render
  // it as plain text with any attachments; no sender label, no reply. The core
  // records no copy, so nothing can reply to it or bridge it back.
  bool system_note = 14;
```

## Commands you will need

Same as 045 (cargo per crate with `nice -n 19` and `CARGO_BUILD_JOBS=2`; Go one
module at a time with `GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19`; whatsapp-rust
only in Step 6 with `RUSTC_BOOTSTRAP=1`), plus:

```sh
(cd connectors/telegram && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go vet ./... && GOMAXPROCS=2 GOFLAGS=-p=2 nice -n 19 go test ./...)
```

## Scope

In: the three protos and `proto/go`; `crates/wuphf-model` (`chat.rs`,
`dispatch.rs`, `routing.rs`, `connector.rs`); `crates/wuphf-storage`
(`lib.rs`, `memory.rs`, `sqlite.rs`, `shared.rs`, `tests.rs`,
`migration_tests.rs`); `crates/wuphf-core` (`profile.rs`, new `notes.rs`,
`ingest.rs`, `router.rs`, `acknowledge.rs`, `lib.rs`, `tests.rs`,
`Cargo.toml` for `sha2`); `crates/wuphf-runtime` (`lib.rs`, `grpc.rs`,
`tests.rs`); `crates/wuphf-connector` (new `media.rs`, `lib.rs`, tests);
`connectors/telegram/internal/telegram/{sender.go,sender_test.go}`;
`connectors/whatsapp/internal/whatsapp/*`; `connectors/whatsapp-rust/{Cargo.toml,Cargo.lock,src/*}`;
`docs/connector-protocol.md`, `docs/architecture.md`, `README.md`,
`connectors/whatsapp/README.md`, `plans/README.md`.

Out: pin / mute / archive (047); deleting mirrored messages when they
disappear on WhatsApp; showing avatars in the TUI or panel (stored only);
member lists; one-to-one Telegram chats'
photos or descriptions; local config and secret files.

## Git workflow

Commit directly on `v2`, one commit per step. Do not push. No branches.

## Steps

### Step 0: Baseline

Drift check. `nice -n 19 cargo test --workspace` → record the count. Go:
record `go test ./...` for `connectors/shared`, `connectors/whatsapp`,
`connectors/telegram`, one at a time. Do not build whatsapp-rust.

### Step 1: The contract

Apply "Contract additions" above. Fill the new prost fields with neutral
values at every Rust literal site so the workspace compiles
(`pb::ChatUpdate { title: None, description: None, avatar: None,
participant_count: None, participant_changes: Vec::new(), .. }`,
(and any `pb::ParticipantChange` literal gets `actor: None, actor_name: None`)
`pb::ChatUpdateEvent { snapshot: false, .. }`,
`pb::MessageCreateDispatch { system_note: false, .. }`), including the
whatsapp-rust literals 045 added (not built until Step 6; say so in the commit
body). Regenerate the Go bindings.

**Verify**: `git status --short proto/go` → only `client.pb.go` and
`server.pb.go` modified. `nice -n 19 cargo check -p wuphf-proto -p
wuphf-connector -p wuphf-runtime --all-targets` → exit 0. `proto/go` build +
vet, and `connectors/whatsapp` and `connectors/telegram` `go vet ./...` → exit
0.
Commit: `feat(proto): chat profile fields, membership changes and system notes`.

### Step 2: Model and storage

Model (`crates/wuphf-model`). Every new field on a stored type is
`#[serde(default, skip_serializing_if = …)]`, so 045's stored profiles,
existing conversations and the migration fixtures keep their exact JSON:

- `chat.rs`:
  ```rust
  pub struct ChatProfile {
      pub connector_id: ConnectorId,
      pub chat_id: String,
      pub disappearing: Option<DisappearingTimer>,       // 045
      pub title: Option<String>,
      pub description: Option<String>,
      pub avatar: Option<ChatAvatar>,
      pub participant_count: Option<u32>,
  }
  /// `id` empty = removed. `hash` is the core's SHA-256 (hex) of the bytes.
  pub struct ChatAvatar { pub id: String, pub media_id: Option<MediaId>, pub hash: Option<String> }
  pub struct ReportedAvatar { pub id: String, pub media_id: Option<MediaId> }
  pub enum ParticipantChangeKind { Joined, Left, Promoted, Demoted }
  pub struct ParticipantChange {
      pub kind: ParticipantChangeKind,
      pub participant: String,
      pub name: Option<String>,
      pub actor: Option<String>,        // canonical id, same form as `participant`
      pub actor_name: Option<String>,
  }
  pub struct ChatUpdate {
      pub chat_id: String,
      pub disappearing: Option<DisappearingTimer>,       // 045
      pub title: Option<String>,
      pub description: Option<String>,
      pub avatar: Option<ReportedAvatar>,
      pub participant_count: Option<u32>,
      pub participant_changes: Vec<ParticipantChange>,  // skip when empty
  }
  ```
- `connector.rs`: `InboundWork::ChatUpdate { connector_id, updates, snapshot:
  bool }`.
- `dispatch.rs`: `Action::Note { text: String, attachments: Vec<Attachment> }`
  — doc: a system note the core composed; acknowledging it records no copy.
  `message_id()` → `None`; `records_copy()` → `false`. `shared.rs`
  `bind_native_parent`: a no-op arm with the comment that a note is never
  blocked.
- `routing.rs`: `Conversation.introduced: bool` (`#[serde(default,
  skip_serializing_if = "std::ops::Not::not")]`) — "the info card was created
  for this conversation; never post another".

Storage (`crates/wuphf-storage`):

- `TopologyStore::mark_conversation_introduced(&self, id: ConversationId) ->
  StorageResult<()>` — a narrow writer like `set_conversation_title`
  (SQLite: `UPDATE conversations SET payload = json_set(payload,
  '$.introduced', json('true')) WHERE id = ?`; memory: set the flag).
- Media pruning spares media a chat profile references. SQLite: add
  `AND NOT EXISTS (SELECT 1 FROM chat_profiles WHERE chat_profiles.payload
  ->> '$.avatar.media_id' = media_blobs.id)` to the `DELETE`; memory: the same
  check over `chat_profiles`. No schema change, so **no migration**: the new
  data lives in JSON payloads 045 already created.

Tests:
1. `tests.rs`, both backends: `chat_profiles_round_trip_every_field` (all new
   fields, then a put with fewer fields replaces the row);
   `marking_a_conversation_introduced_keeps_the_rest` (title and endpoints
   unchanged, flag set); `pruning_spares_media_a_profile_references` (two old
   unreferenced blobs, one named by a profile's avatar → only the other is
   pruned).
2. `migration_tests.rs`: `chat_profiles_from_045_decode` — a database migrated
   to 0019 with a 045-shaped row (`{"connector_id":…,"chat_id":…,"disappearing":{…}}`)
   decodes with the new fields `None`; `conversations_without_introduced_decode`
   (a 0014-shaped conversation → `introduced: false`). Extend
   `real_database_migrates_and_decodes` if it does not already decode
   `chat_profiles`.
3. Model: a `ChatProfile::new(..)` and a `Conversation` with `introduced:
   false` serialize without the new keys; a `ParticipantChange` without an
   actor serializes without `actor` / `actor_name`; an `Action::Note` neither records a
   copy nor has a message id (extend the `actions()` table in `dispatch.rs`).

**Verify**: `nice -n 19 cargo test -p wuphf-model -p wuphf-storage` → green.
Mutation: drop the profile clause from the SQLite prune → test 1's pruning
case fails on `sqlite_` only (parity broken), restore.
Commit: `feat(storage): richer chat profiles, introduced conversations, avatar media kept`.

### Step 3: The core turns profile changes into notes and cards

New `crates/wuphf-core/src/notes.rs` — pure composition, the only place the
wording lives:

```rust
pub(crate) struct Note { pub text: String, pub attachments: Vec<Attachment> }

/// "24 hours", "7 days", "90 days"; a whole number of days when divisible,
/// else hours, else seconds. 0 → "off".
pub(crate) fn timer_text(seconds: u32) -> String;

/// The info card: title line, then (each only when known) the description,
/// "⏳ Disappearing messages: …", "👥 N participants"; the avatar attached.
pub(crate) fn info_card(title: &str, profile: Option<&ChatProfile>, avatar: Option<Attachment>) -> Note;

/// One note per change, in this order: description, avatar, timer, then each
/// membership change. Wording:
///   "📝 Description changed:\n<text>" / "📝 Description removed"
///   "🖼️ Photo changed" (photo attached) / "🖼️ Photo removed"
///   "⏳ Disappearing messages: 7 days" / "⏳ Disappearing messages: off"
///   with an actor other than the member:
///     "➕ <actor> added <name>" / "➖ <actor> removed <name>" /
///     "⭐ <actor> made <name> an admin" / "<actor> dismissed <name> as admin"
///   without one, or when `actor == participant`:
///     "➕ <name> joined" / "➖ <name> left" /
///     "⭐ <name> is now an admin" / "<name> is no longer an admin"
///   A missing name falls back to the id (actor_name to actor likewise).
pub(crate) fn change_notes(changes: &ProfileChanges, avatar: Option<Attachment>) -> Vec<Note>;
```

`profile.rs` (extend 045's module):

- `ProfileChanges { title: Option<String>, description: Option<String>,
  avatar: Option<ChatAvatar>, disappearing: Option<DisappearingTimer>,
  participants: Vec<ParticipantChange> }` — only fields whose stored value was
  **known and differed** (decision 2: a first-seen value is a silent baseline).
- `pub(crate) fn apply(profile: &mut ChatProfile, update: &ChatUpdate, avatar_hash:
  Option<String>) -> ProfileChanges` replaces 045's `apply -> bool`
  (`changed()` on the result keeps 045's callers working):
  - `disappearing`: 045's `merge_disappearing`; a change counts only if the
    stored value was `Some`.
  - `title`, `description`: set; a change when the stored value was `Some`
    and differs.
  - `avatar`: compare **hashes** (the caller hashes the reported media); an
    equal hash keeps the stored id and media id; a removal (`id` empty) is a
    change when a picture was stored.
  - `participant_count`: an explicit count replaces; otherwise each
    `Joined` / `Left` adjusts a known count (saturating). Never a note by
    itself.
  - `participant_changes`: copied into the changes (they are events).
- `Core::update_chats(&self, connector_id, updates, snapshot) ->
  Result<Vec<Dispatch>, CoreError>` (was `-> usize`): for each update, hash
  the reported avatar (`storage.media(id)` → SHA-256 hex of the bytes; a
  missing blob → treat as no avatar report), apply, store when anything
  changed, then:
  - a title change, or a first title, goes through the retitle path (below)
    — snapshots included, since a title is not a note;
  - unless `snapshot`, each change note becomes a `Note` dispatch to every
    other endpoint of each conversation the chat belongs to (below).
  `Core::observe_message_timer` (045) returns the same note dispatches when a
  message carries a timer that changes a known one; `ingest_create` appends
  them to its outcome.
- **Which conversations.** `async fn conversations_of_chat(&self, connector,
  chat_id) -> Vec<(Conversation, EndpointId)>`: for every form from
  `address_forms` (045), `resolve_endpoint` → `conversation_for_endpoint`;
  dedup by conversation id. Unrouted chats yield nothing (the profile is still
  stored).
- **Retitle.** Refactor `maybe_retitle` (`ingest.rs:149-184`) so its body after
  the origin check is `retitle_conversation(&routing, conversation, title)`,
  used by both the message path and `update_chats` (which requires the
  updated chat to be the conversation's source, like the message path).
- `Router::route_note(&self, conversation, from: EndpointId, note: &Note) ->
  Vec<Dispatch>`: `Action::Note` to every endpoint except `from`, skipping a
  mirror endpoint without a thread (`is_mirror && thread_id.is_none()`);
  `Target.create_thread: None`.
- **Info card.** In `route_and_record` (`ingest.rs:232-258`), after routing:
  if any routed dispatch has `create_thread` and the conversation is not
  `introduced`:
  1. `mark_conversation_introduced` (before anything else, so a crash cannot
     post two cards);
  2. build the card from the conversation title (or the provision title) and
     the source chat's profile (`conversation.source` → `chat_profile`, over
     `address_forms`), with the avatar's `Attachment` from
     `storage.media(..)`;
  3. **discard** the routed dispatches (none were saved yet), save the card as
     `Dispatch::new(target_with_create_thread, conversation_id, Action::Note
     {..}, None)`, register it in `self.provisioning`, and stash the message
     with `stash_provisioning_message` (the existing durable stash);
  4. return the card as the only dispatch.
  The ack establishes the thread (`persist_provisioned_thread`) and
  `resume_provisioned` replays the message through `route_and_record`, which
  now finds the thread. A failed card settles the same way and the replay
  provisions with the message (the conversation is already introduced).
- `crates/wuphf-runtime/src/lib.rs` `process_work`: the `ChatUpdate` arm
  forwards the returned dispatches.

Tests (`crates/wuphf-core/src/tests.rs`; reuse 045's WhatsApp DM / group and
Telegram mirror fixtures):

1. `notes::timer_text` and `change_notes` wording (pure table tests).
2. `first_seen_values_are_a_silent_baseline` — a non-snapshot update with a
   description, avatar, count and timer for a chat with no profile → stored,
   no dispatches.
3. `a_description_change_posts_a_note_in_the_mirror_thread` — then a changed
   description → one `Action::Note` to the Telegram endpoint, none to the
   WhatsApp side.
4. `a_snapshot_never_posts_notes` — a snapshot with a changed description →
   stored, no notes.
5. `the_same_photo_is_never_posted_twice` — avatar A (uploaded media) as
   baseline; a new id with the same bytes → no note, stored id unchanged; new
   bytes → one note with the photo attached; removal → "Photo removed".
6. `membership_changes_post_notes_and_adjust_the_count` — count 10 known;
   two joins and a leave → three notes, count 11; a promote → note, count 11.
6a. `membership_notes_name_the_actor` — pure `change_notes` table: each kind
   with an actor → "Alice added Bob" / "Alice removed Bob" / "Alice made Bob
   an admin" / "Alice dismissed Bob as admin"; no actor → "Bob joined" / "Bob
   left" / "Bob is now an admin" / "Bob is no longer an admin"; `actor ==
   participant` → the plain wording; missing names fall back to the ids.
7. `a_title_change_renames_without_a_note` — `RenameThread`, no note; also
   from a snapshot.
8. `a_timer_change_posts_a_note` — known 0 → 604800 via `ChatUpdate` →
   "⏳ Disappearing messages: 7 days"; via a message carrying the timer →
   same note; the 045 unknown fill (None → 0) → no note.
9. `notes_skip_unprovisioned_mirrors` — a conversation whose mirror has no
   thread yet → no note.
10. `a_new_mirror_topic_starts_with_an_info_card` — a profile with all
    fields, then the first message → the only dispatch is a `Note` with
    `create_thread` and the photo; acknowledging it with a thread id → the
    message dispatch follows, into that thread, with no `create_thread`.
11. `one_card_per_conversation` — the card dispatch fails → the replayed
    message provisions the thread itself; a later provisioning (thread lost
    and re-provisioned) posts no second card.
12. `a_note_records_no_copy` — acknowledge a note → no copy at that endpoint
    (`message_id_for_copy` on its native id is `None`), no delivery ack; a
    Telegram reply quoting that native id routes with no reply link.
13. `owner_message_provisioning_also_gets_a_card` — `send_owner_message`
    into a chat whose mirror has no thread → the card first.

**Verify**: `nice -n 19 cargo test -p wuphf-core -p wuphf-runtime` → green.
Mutations: compare avatar ids instead of hashes → test 5 fails; drop
`snapshot` from the note condition → test 4 fails; mark the conversation
introduced only on acknowledgement instead of when the card is created → test
11 fails (the failed card is followed by a second card).
Commit: `feat(core): notes and info cards from chat profile changes`.

### Step 4: Wire mapping and the Rust SDK media client

- `crates/wuphf-runtime/src/grpc.rs`: map the new `ChatUpdate` fields,
  `ParticipantChange` (an `UNSPECIFIED` kind is a `WireError`) with its optional `actor` / `actor_name`, `ChatAvatar`
  (`media_id` parsed as a `MediaId`), `ChatUpdateEvent.snapshot` →
  `InboundWork::ChatUpdate.snapshot`; `Action::Note` →
  `MessageCreateDispatch { system_note: true, sender: "", participant: None,
  sender_name: None, text, attachments, reply_*: None, timestamp_ms: 0,
  disappearing: None, provision_thread from create_thread }`.
- `crates/wuphf-connector`: new `media.rs` with `MediaClient::connect(core_address,
  token)` and `async fn upload(&self, metadata: pb::MediaMetadata, bytes:
  Vec<u8>) -> Result<pb::Attachment, SessionError>` over
  `MediaTransportClient::upload_media` (a metadata frame, then chunks of at
  most 64 KiB, `authorization: Bearer`), mirroring the Go
  `coreclient.DialMedia` / `Upload`. Export it from `lib.rs`.

Tests: grpc mapping tests for each new field and for a note dispatch;
`crates/wuphf-connector/tests/` — upload against the in-process test server
the session tests use (if that harness has no media service, add the hub's
`MediaTransport` from `wuphf-runtime` the way the runtime tests do) → the
returned attachment has the uploaded size.

**Verify**: `nice -n 19 cargo test -p wuphf-runtime -p wuphf-connector` →
green.
Commit: `feat(runtime): carry profile updates and system notes; Rust media upload`.

### Step 5: Telegram renders notes

`connectors/telegram/internal/telegram/sender.go` `SendMessage`: when
`dispatch.GetSystemNote()`, render with no sender header even if a
participant is set (`attributedText("", "", text)`), never reply to a quoted
message (still into the topic root), and never use the sticker-label button.
A note with `provision_thread` creates the topic like any provisioning
dispatch. Everything else (attachments, albums, over-length text as a file)
is the normal path.

Tests (`sender_test.go`): a note with a participant set renders no header; a
note with a photo goes through the photo path with the text as caption; a
note with `provision_thread` creates the topic and returns its id.

**Verify**: `connectors/telegram` `go vet ./...` and `go test ./...` → green.
Commit: `feat(telegram): render system notes`.

### Step 6: whatsapp-rust — contacts, names, mentions, profile reports

Build once at the end (`cargo test`, then `cargo clippy`), alone.

1. **Dependencies.** Add `diesel = { version = "=2.3.11", default-features =
   false, features = ["sqlite"] }` and `whatsapp-rust-sqlite-storage = { git =
   "https://github.com/oxidezap/whatsapp-rust", rev = "5ca80c5c" }` — both
   already in the lock, so the lock gains only the root's dependency edges
   (verify below). Hashing stays in the core; the connector adds no hash crate.
2. **`contacts.rs` (new): the `wuphf_contacts` table, through the session
   store's shared pool.** Why the shared pool rather than a separate
   connection: it is the library's documented path for sibling tables
   (`SqliteStore::shared`); it reuses the store's diesel/r2d2 pool and
   semaphore, so the two never contend for SQLite's write lock from separate
   pools; and a separate connection would need another SQLite driver —
   rusqlite links its own `libsqlite3-sys`, which must equal the 0.37 diesel
   uses or the build fails (the reason this connector is a separate
   workspace). Schema, created at startup with `CREATE TABLE IF NOT EXISTS`
   through `shared().run(..)` with `diesel::sql_query` (no diesel migration
   entries, so the store's own migration table is untouched):
   ```sql
   CREATE TABLE IF NOT EXISTS wuphf_contacts (
       jid TEXT PRIMARY KEY NOT NULL,   -- canonical (non-AD) JID string
       full_name TEXT,
       first_name TEXT,
       push_name TEXT,
       updated_at INTEGER NOT NULL      -- Unix seconds
   );
   ```
   `Contacts { shared: SharedSqlite }` with `async fn upsert_push_name(jid,
   name)`, `async fn upsert_contact_name(jid, full, first)`, `async fn
   get(jid) -> Option<ContactNames>` (looked up under the JID and, for a user
   or LID JID, its other form via `client.get_lid_pn_entry`). Writes skip
   unchanged values (read first) so a push name on every message costs one
   indexed read.
3. **Filling it.** `Normalizer::handle`: `upsert_push_name(sender,
   info.push_name)` for every message not from me (before the title is
   resolved). `ChatEvents`: `Event::ContactUpdate` → `upsert_contact_name(jid,
   action.full_name, action.first_name)` (and the `pn_jid` / `lid_jid` form
   when present); `HistorySync` → every `pushnames` entry, **before** the
   conversations are mapped, so the snapshot titles use them.
4. **Names** (`names.rs`): `format_person_name(number, names)` becomes the
   Go order without the business step: saved full name alone; else push name
   + " (number)"; else the number. `dm_title(is_from_me, number, names)`: the
   formatted name for either side when `names` has a full or push name — a DM
   the owner starts now gets its contact's name; `None` only when nothing is
   known (045's no-churn rule still holds: a title appears only from real
   data). `sender_name` (`normalize.rs`) uses the same lookup for group
   senders ("Me" for own messages stays).
5. **Mentions** (`normalize.rs`): for each `context_info.mentioned_jid`,
   replace `@<user part>` in the text with `@<name>` (contacts lookup, push
   name, else unchanged). Pure helper `render_mentions(text, &[(user,
   name)])`, unit-tested.
6. **Profile reports** (`chats.rs`):
   - `GroupUpdate`: `Subject { subject, .. }` → `title = tagged("G",
     subject)`; `Description { description, .. }` → `description =
     description.unwrap_or_default()`; `Add` / `Remove` / `Promote` / `Demote`
     → one `ParticipantChange` per participant (`canonical(jid)`, name from
     contacts, else `display_name`, else the phone number). The actor is
     `GroupUpdate.participant` / `participant_pn` (`wacore/src/types/events.rs:1375-1390`):
     take the one in the members' address form (`is_lid_addressing_mode` →
     `participant`, else `participant_pn` when present, else `participant`),
     `actor = canonical(..)`, `actor_name` resolved like a member name. 045's
     `Ephemeral` handling stays. Every `GroupUpdate` still calls `groups.forget`.
   - `PictureUpdate { jid, removed, picture_id, .. }` → `avatar`: removed →
     `ChatAvatar { id: "" }`; otherwise fetch and upload (item 7).
   - `ContactUpdate` for a user JID → after the table update, a `title`
     update for that DM (`dm_title(false, …)`), so a saved-name change
     renames the topic.
   - `HistorySync` (extend 045's mapping): `title` for each conversation —
     groups `tagged("G", name)` when the conversation has a `name`, DMs from
     the contacts table — sent with `snapshot: true`.
7. **Avatar fetch.** `avatars.rs` (new): `async fn fetch(&self, jid) ->
   Option<ReportedAvatar>`: `client.contacts().get_profile_picture(&jid,
   false)` → `None`/no picture → `ChatAvatar { id: "" }`; else GET `url`
   through `client.http_client` (`HttpRequest::get`), upload with
   `wuphf_connector::MediaClient` (`MediaMetadata { kind: KIND_IMAGE,
   mime_type: "image/jpeg", file_name: "avatar.jpg", size_bytes }`) →
   `{ id: picture.id, media_id }`. A failed download or upload → `None` (the
   update is sent without an avatar; logged at `warn`).
8. **First sighting.** `Sightings` (a `Mutex<HashSet<String>>` of chat ids
   this process has reported): in `Normalizer::handle`, before the message
   frame, the first time a chat is seen send a snapshot update with `title`,
   `description` and `participant_count` (groups: from the `GroupDirectory`,
   which now also keeps `description` and the participant count), and the
   avatar from item 7. Emitted from the normalizer (an event handler), so an
   awaited send on the input channel is fine there. **Never** emit from
   `WhatsAppSender`: that runs inside the session loop that drains the
   channel (045's hazard).
9. **Notes on send.** `sender.rs`: a `system_note` dispatch is plain text
   (this connector still sends no attachments: a note with an attachment
   sends its text only, and that is acknowledged, not dead-lettered — a note
   is informational). 045's timer stamping applies as for any text.

Tests (pure, no client): `format_person_name` / `dm_title` order (full name;
push + number; number; own DM with a known contact → the contact's title;
nothing known → `None`); `render_mentions`; the `GroupUpdate` → `ChatUpdate`
mapping for `Subject`, `Description` (and `None` → empty), each membership
action with and without an actor (LID-mode group → `participant`; PN-mode →
`participant_pn`; a self-leave reports actor == member); the history-sync mapping (push names first, group names tagged,
`snapshot: true`); the contacts SQL against an in-memory diesel
`SqliteConnection` (`:memory:`, same `CREATE TABLE`), including the
unchanged-value skip and the LID/phone lookup helper with a fake mapping.

**Verify**: `(cd connectors/whatsapp-rust && RUSTC_BOOTSTRAP=1
CARGO_BUILD_JOBS=2 nice -n 19 cargo test)` → green; clippy (`-D warnings`)
clean; `git diff connectors/whatsapp-rust/Cargo.lock` shows only the root
package's dependency list gaining `diesel` and `whatsapp-rust-sqlite-storage`
(no new `[[package]]` entries). Mutation: report titles before push names in
the history-sync mapping → the history-sync test fails.
Commit: `feat(whatsapp-rust): contacts, names, mentions and chat profile reports`.

### Step 7: Go WhatsApp — profile reports and mentions

1. **Group events** (`updates.go` `HandleGroupInfo`, extend 045's): `Name !=
   nil` → `title = tagged("G", Name.Name)` (and drop the namer's cache entry
   for the group); `Topic != nil` → `description = Topic.Topic` (`""` when
   `TopicDeleted`); `Join` / `Leave` / `Promote` / `Demote` → one
   `ParticipantChange` each, `participant = n.canonical(ctx, jid)`, `name =
   namer.PersonName(ctx, jid, "")`. The actor is `evt.Sender`
   (`*types.JID`, `types/events/events.go:464`; absent for invite-link joins):
   `actor = n.canonical(ctx, *evt.Sender)` (the same canonicalization as the
   members, so the self check compares like with like), `actor_name =
   namer.PersonName(ctx, *evt.Sender, "")` (`PersonName` already maps a LID
   to its phone number; `evt.SenderPN`, `events.go:465`, is not needed). One
   update per event, `snapshot: false`.
2. **Pictures** (`connector.go` switch + `profiles.go`, new): `*events.Picture`
   → `avatar`: `Remove` → `{Id: ""}`; else `GetProfilePictureInfo(ctx, JID,
   nil)` → HTTP GET of `URL` (a 30-second timeout) → `MediaBroker.Upload`
   (`KIND_IMAGE`, `image/jpeg`) → `{Id: info.ID, MediaId}`. Failure → no
   avatar field, logged.
3. **Names** (`*events.Contact`, `*events.PushName`, `*events.BusinessName`):
   for a user JID, drop the namer's cached title for that chat (`chatNamer`
   gains `Forget(chat)`) and send `title = namer.ChatTitle(ctx, chat)` for the
   DM (canonical chat id). `Contact` with `FromFullSync` → `snapshot: true`.
4. **History sync** (extend 045's `HandleHistorySync`): `title` per
   conversation — groups `tagged("G", conv.GetName())` when non-empty (no
   `GetGroupInfo` per chat: hundreds of network calls), DMs
   `namer.ChatTitle` (local contact store; whatsmeow has already stored the
   sync's push names) — and `Snapshot: true` on every frame.
5. **First sighting** (`profiles.go`): `chatProfiler` with a seen-set, called
   from `Normalizer.Handle` before the create frame: groups → `GetGroupInfo`
   (reuse the namer's call: `chatNamer` keeps the last `*types.GroupInfo`
   alongside the title) for `Topic`, `len(Participants)`; every chat → the
   avatar (item 2); plus the title. One snapshot frame.
6. **Mentions** (`updates.go` `normalize`): for each
   `messageContext(..).GetMentionedJID()`, replace `"@" + jid.User` in the
   text with `"@" + namer.PersonName(ctx, jid, "")`. Pure helper
   `renderMentions(text string, names map[string]string) string`.
7. **Notes on send** (`sender.go`): a `system_note` is sent like any content
   (text and attachments), with 045's timer stamping and without a quoted
   context.

Tests: `TestHandleGroupInfoReportsNameTopicAndMembers` (title tagged, topic
and deleted topic, each membership kind with names from `stubNamer`, with a
`Sender` → actor id and name, without one → no actor, and `Sender` equal to
the member → actor equal to participant);
`TestPictureEventUploadsTheAvatar` (fake picture client and HTTP server via
`httptest`, fake broker; removal → empty id; failed download → no avatar);
`TestContactEventsRetitleTheDM` (cache dropped, new title sent; `FromFullSync`
→ snapshot); `TestHistorySyncReportsTitlesAsSnapshot`;
`TestFirstSightingSendsOneSnapshotBeforeTheMessage` (frame order, second
message → no snapshot); `TestRenderMentions`; `TestSendRendersASystemNote`.

**Verify**: `connectors/whatsapp` `go vet ./...` and `go test ./...` → green.
Mutation: send the first-sighting snapshot after the create frame → the
order test fails.
Commit: `feat(whatsapp): chat profile reports, avatars and mention names`.

### Step 8: Docs

- `docs/connector-protocol.md`: the new `ChatUpdate` fields and their rules
  (title = the display title; description empty = cleared; avatar id empty =
  removed; membership changes are events, never in a snapshot), `snapshot`,
  `system_note` (render plain, no label, no reply; the core records no copy).
- `docs/architecture.md`: extend 045's "Chat Profiles" section — baseline vs
  change, notes, the info card provisioning the topic, avatar hashing and
  pruning, the count without a member list.
- `README.md` and `connectors/whatsapp/README.md`: one bullet each.

Commit: `docs: chat profiles, notes and info cards`.

### Step 9: Gate and index

- `nice -n 19 cargo test --workspace` → Step 0 count + new tests; clippy and
  fmt clean.
- Go one module at a time: `proto/go` build + vet; `connectors/shared`,
  `connectors/whatsapp`, `connectors/telegram` test; `connectors/instagram`
  build.
- `plans/README.md`: row 046 DONE with notes (counts, mutations, deviations).
- Commit: `docs(plans): mark plan 046 done`.

## Test plan

| Test | Where | Proves |
|---|---|---|
| profile / introduced / prune parity, 045-shape decodes | wuphf-storage | storage, no migration needed |
| baseline, snapshot, description, photo hash, membership (with and without actor), title, timer, unprovisioned | wuphf-core | decisions 1, 2, 5, 6, 7 |
| info card, one card, owner provisioning | wuphf-core | decision 4 |
| note records no copy | wuphf-core | decision 3 |
| grpc mapping, media upload | wuphf-runtime, wuphf-connector | the wire |
| note rendering | telegram | no label, photo path |
| contacts, names, mentions, mappings | whatsapp-rust | decision 8, 9 |
| group / picture / contact / history / first sighting / mentions | Go whatsapp | decision 9 |

## Done criteria

- [ ] Every test above passes; every listed mutation fails a test.
- [ ] No SQL migration added; 045-shaped rows decode.
- [ ] whatsapp-rust built once; its lock gained no packages.
- [ ] Docs updated; row 046 DONE.

## STOP conditions

- The drift check fails, or 045 is not DONE.
- Regenerating the Go bindings changes files other than `client.pb.go` and
  `server.pb.go`.
- whatsapp-rust at `5ca80c5c` lacks `SqliteStore::shared`, `PictureUpdate`,
  `ContactUpdate`, `GroupNotificationAction::{Subject, Description, Add,
  Remove, Promote, Demote}` or `contacts().get_profile_picture`; whatsmeow
  lacks `events.Picture` or `GetProfilePictureInfo`.
- Adding `diesel` or `whatsapp-rust-sqlite-storage` to the connector adds a
  package to its lock (a version mismatch).
- An existing migration fixture test fails (a new field serializes when
  empty).

## Maintenance notes

- **Never emit frames from a `Sender`** (Go or Rust): the session loop that
  calls it is the one draining the input channel. Profile reports come from
  normalizers and event handlers only.
- **chat-store**: if whatsapp-rust's chat-store crate ever gains an option to
  skip storing messages (a `store_messages` switch), switching the Rust
  contacts table to it is possible; `wuphf_contacts` was named to leave its
  `contacts` table free.
- **The contacts table has no migration runner.** A later schema change
  needs one (a `wuphf_schema_version` table, or diesel migrations with
  versions that cannot clash with the store's).
- **Notes are never echoed** because both WhatsApp clients do not deliver
  their own sends back and the Telegram connector drops its own outgoing
  messages (`updates.go:168`). A connector that reports its own sends would
  bridge notes back; it must drop them.
- **Avatar uploads per process start** (first sighting) become orphans when
  the hash is unchanged and are pruned after `orphan_media_retention_seconds`.
- Slice 3 (047) adds pin / mute / archive fields to `ChatProfile` and
  `ChatUpdate` the same way: optional, skipped when empty.
