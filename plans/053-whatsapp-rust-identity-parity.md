# Plan 053: whatsapp-rust — canonical ids, identity migrations, the addressing book, quote authors and channel titles

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Build limits (the owner's laptop)**:
> - Prefix every `cargo` and `go` command with `nice -n 19` and set
>   `CARGO_BUILD_JOBS=2` (Go: `GOMAXPROCS=2 GOFLAGS=-p=2`). Check and test only
>   the crates you touch (`-p <crate>`); never the whole workspace.
> - The whatsapp-rust connector (`connectors/whatsapp-rust`) is its own Cargo
>   workspace and the heaviest build in the repo. Every cargo command for it
>   needs `RUSTC_BOOTSTRAP=1` and `--manifest-path
>   connectors/whatsapp-rust/Cargo.toml`, and it runs alone: never while any
>   other build runs.
> - Before **every** build or test command, run
>   `pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv`.
>   If it prints anything, another build is running: wait, re-checking every
>   60 seconds, until it prints nothing.
> - One build at a time. No `nix flake check`. No release builds except the
>   dev-binary rebuild in the last step.
> - The last step rebuilds the touched dev binaries into
>   `scripts/dev-bin/.bin/`, mirroring `scripts/install-dev-bins.sh`, one at a
>   time and niced, because the owner live-tests with them.
> - Stage explicit paths only (`git add <path> …`; never `git add -A`, `.` or
>   `-u`): another session may be working in the same tree. Run
>   `git status --short` before each commit and commit only this step's files.
>
> **Drift check (run first)**: plans that must be DONE: none (051 is a soft
> dependency: if it is DONE, `normalize.rs` reads the base message and
> `Resolved` carries `attachments`; keep both). Run
> `git log --oneline 313038d..HEAD -- crates/wuphf-connector crates/wuphf-proto crates/wuphf-core connectors/whatsapp-rust connectors/whatsapp/internal/whatsapp`.
> Expected: no commits, or only commits of plans 051, 052 and 054–056 that
> `plans/README.md` marks DONE. Any other commit: re-check every fact below.
> What must still hold (if any is false, STOP; a fact a DONE plan changed as
> its own plan says counts as holding, and line numbers may have moved by
> those plans' edits):
> - `connectors/whatsapp-rust/src/normalize.rs:205-207`: `canonical(jid) =
>   jid.to_non_ad().to_string()` — device stripped, nothing else. It names
>   the chat (216), the reactor (278), the sender and participant (297, 308)
>   and the first-sighting chat (139). `with_other_forms` (189-201), called at
>   71-73, adds the DM's other form to every chat update the normalizer sends.
> - `connectors/whatsapp-rust/src/chats.rs`: `chat_forms` (405-423) returns the
>   canonical id plus the other form from `get_lid_pn_entry`; picture updates
>   (146) and contact title updates (185) are reported under every form;
>   `group_profile_update` (227-276) names members and the actor by
>   `canonical`. `disappearing.rs:98-131` `history_updates` adds each
>   conversation's `pn_jid` / `lid_jid` form (112-118).
> - `connectors/whatsapp-rust/src/sender.rs:134-151`: `quote_context` always
>   names the paired account's phone JID as the quoted participant, with the
>   comment "The quoted copy at this endpoint is always bridge-authored";
>   `parse_chat` (32-36) is the only address handling.
> - `connectors/whatsapp-rust/src/names.rs:33-50`: `title` returns `None` for
>   anything but groups and DMs (48).
> - `crates/wuphf-proto/client.proto:115-120` `IdentityMigrated { old_chat_id,
>   old_thread_id, new_chat_id, new_thread_id }`; `ClientFrame` carries it; the
>   core links the two forms (`crates/wuphf-core/src/resolve.rs`, `Core::address_set`
>   and 045's `address_forms`), so chat profiles (`target_disappearing`) and
>   notes (`conversations_of_chat`) are found under any linked form.
> - whatsapp-rust library (`~/.cargo/git/checkouts/whatsapp-rust-6e26428647c827f3/5ca80c5`):
>   `src/client/lid_pn.rs:947` `get_lid_pn_entry(&Jid) -> Result<Option<LidPnEntry>>`
>   (cache-aside, no network); `:623` `pub async fn is_lid_migrated(&self) ->
>   bool`; `:660-671` `resolve_dm_wire_jid` (the library already sends a DM in
>   the account's wire namespace). `src/client/accessors.rs:247` `get_pn()`,
>   `:251` `get_lid()`. `wacore/src/types/message.rs:104-114` `MessageSource {
>   chat, sender, is_from_me, is_group, addressing_mode: Option<AddressingMode>,
>   sender_alt, recipient_alt, .. }`, `:23-29` `AddressingMode { Pn, Lid }`.
>   `src/features/groups.rs:99` `GroupMetadata.addressing_mode`.
>   `src/features/newsletter.rs:190` `client.newsletter().get_metadata(&jid)
>   -> Result<NewsletterMetadata { name, .. }>` (116-128).
>   `wacore/src/proto_helpers.rs:695` `build_quote_context(message_id,
>   sender_jid: impl Into<String>, quoted)`.
> - Go WhatsApp (the reference, `connectors/whatsapp/internal/whatsapp/`):
>   `identity.go:28-60` (`identityResolver`), `:57-68` `Canonical`, `:70-81`
>   `Alternate`, `:86-107` `cached`, `:114-142` `migrationGate`;
>   `updates.go:99-118` (`Handle`: canonicalize once, `Observe`, migrations
>   first), `:184-217` `migrationsFor`, `:482-499` `identityMigration`,
>   `:664-703` `HandleHistorySync` (canonical ids, no alternates);
>   `addressing.go:21-172`; `sender.go:231-295` (`quotedContext`,
>   `quotedAuthor`, `target`); `names.go:65-74, 125-131` (`[C]` channel
>   titles).

## Status

- **Priority**: P1 — the two connectors key the same chat under different
  ids, so switching a connector id between implementations splits every
  LID-addressed DM into a second conversation; replies the bridge sends quote
  the wrong author
- **Effort**: M–L
- **Risk**: MED — every chat and sender id the Rust connector reports can
  change form once; identity migrations bind the old and new forms in the core
- **Depends on**: — (051 soft)
- **Category**: bug / parity
- **Planned at**: commit `313038d`, 2026-10-31

## Why this matters

WhatsApp names one person by a phone JID, a LID and per-device forms of
either, switching from message to message. The Go connector promotes a phone
JID to its LID whenever the mapping is known, reports `IdentityMigrated` so
the core links the forms, and addresses outbound frames in the form the chat
uses. The Rust connector only strips the device. So:

- the same chat gets different chat ids from the two connectors, and the
  owner's settled goal — switch a connector id between the Go and the Rust
  implementation and keep every conversation — fails for every DM whose LID
  is known;
- a DM that arrives under its phone JID one day and its LID the next mints a
  second endpoint and a second mirror topic;
- a reply bridged from Telegram that quotes a message the contact sent names
  the owner's own phone JID as the quoted author (`sender.rs:134-151`), so
  WhatsApp shows the reply unlinked; in a LID-addressed chat even the
  bridge's own messages are quoted under the wrong form;
- channels (newsletters) get no title, where Go shows `[C] <name>`.

Plan 3 of 6 (051–056) bringing the Rust connector to parity with the Go one.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Canonicalization identical to Go's `identity.go`: device always stripped; a phone JID promoted to its LID when the mapping is known; groups, LIDs, channels and broadcasts unchanged; hits cached for the process, misses not. Uses whatsapp-rust `get_lid_pn_entry` and `MessageSource.addressing_mode` / `sender_alt` / `recipient_alt`. |
| 2 | `IdentityMigrated` frames exactly like Go's `migrationsFor`: WhatsApp's alt-address hint for the other party (owner Q50, 2026-10-31: `sender_alt` on a message they sent, `recipient_alt` on one the account sent; LID form canonical; Go reads only `recipient_alt` and gets the same follow-up), the shift from a raw phone form to its canonical LID, and the alternate the LID store holds — DMs only, deduped per process, sent **before** the message's own frames. |
| 3 | The quoted participant mirrors Go's `quotedAuthor`: when the quoted copy was not written by the bridge (`!authored_by_bridge`), its participant (else sender) in the chat's address form; otherwise the paired account's own LID or phone JID, by the chat's addressing mode. |
| 4 | The outbound addressing book is ported from Go's `addressing.go` (`Observe`, `Mode`, `InMode`, `Own`). |
| 5 | Channel titles are `[C] <name>` from `client.newsletter().get_metadata`. |
| 6 | 045/046's dual-form chat-update reporting is decided here (see "Reconciling 045/046" below). |

### Reconciling 045/046: chat updates go out under the canonical id only

045 made the Rust connector report every DM chat update under both its LID
and phone forms (`normalize.rs:189-201`, `chats.rs:146, 185`, the history
sync's `pn_jid` / `lid_jid` forms), with this justification (045 "Further
decisions"): "It never sends `IdentityMigrated`, so the core has no address
links for its chats; without this, a timer learned under one form would not
reach a chat addressed under the other." This plan removes that reason, and
keeping the dual reports would now do harm:

- With migrations, the core links the forms, and both readers of chat
  profiles walk every linked form: `target_disappearing` (045) and
  `conversations_of_chat` (046, through `address_forms`). One report under
  the canonical id reaches every conversation the chat belongs to.
- Once the forms are linked, two reports of one change land as two separate
  profile rows that both resolve to the same conversation, so a description,
  photo or timer change would be **noted twice** in the mirror topic (each
  row has its own known-and-differs baseline).
- The Go connector reports chat updates under the canonical id only
  (`updates.go:135, 653-703`); parity means the same ids on the wire.

So: every chat update, first-sighting snapshot, history-sync entry, picture
and contact update is reported once, under the canonical id. Profiles stored
earlier under the other form stay in the core and are harmless (a linked form
is read only when its setting timestamp is strictly newer). `chat_forms`
stays, for the connector's own contacts-table lookups only.

### Planner decisions (within the settled scope)

- **`canonical` is renamed `bare_id`** (it only strips the device), and the
  identity resolver's `canonical` becomes the one that names chats and people
  on the wire. Keeping the old name for the weaker function would invite the
  next change to use the wrong one.
- **Two traits keep the logic testable without a client**: `LidStore`
  (phone ↔ LID lookups) and `Account` (own addresses, LID migration, a
  group's addressing mode), both implemented for `whatsapp_rust::Client`,
  with one shared test fake. Go uses the same seams (`identityResolver`,
  `accountIdentity`).
- **The contacts table keeps its keys** (bare ids, looked up under both forms
  through `chat_forms`): it is the connector's local store, never on the
  wire, and needs no migration.
- **Sender ids, reactors, receipt observers and group members are
  canonicalized too**, as Go canonicalizes every id it reports
  (`updates.go:356, 445, 309`).
- **The library already sends a DM in the account's wire namespace**
  (`resolve_dm_wire_jid`), so the ported `target()` translation changes
  nothing for DMs today; it is ported anyway so the quote participant and
  the chat use the same form, as in Go, and so a library change does not
  silently change the behaviour.

## Current state

See the drift check for file facts. In addition:

- `Normalizer::handle` (`normalize.rs:56-88`) has the client per call
  (`ctx.client`, 045's "client per call" deviation: the bot's `Client`
  exists only after `build()`); `ChatEvents::handle` gets it as an argument;
  `WhatsAppSender` holds `Arc<Client>` (`handle.client()`, `main.rs:181`).
- Go's tests for this area: `addressing_test.go` (8),
  `TestHandleCanonicalizesChatAndEmitsMigrationOnce`,
  `TestHandleReportsThePhoneFormOfALIDAddressedChat`,
  `TestIdentityMigrationTreatsLIDAsCanonical`,
  `TestIdentityMigrationIgnoresGroupAndAbsentAlt`,
  `TestNormalizeStripsSenderDeviceFromIdentity`,
  `TestNormalizerReportsTheFormEachChatArrivesIn`,
  `TestQuotedAuthorAttributesTheRealSender`,
  `TestReplyQuotesAuthorInTheChatsForm`, `TestDirectChatIsAddressedInItsForm`,
  `TestSendDeliversToCanonicalLIDTarget`, `names_test.go` (2).
- 045's whatsapp-rust test for the history-sync mapping asserts that the
  `pn_jid` form is added; Step 2 changes that expectation on purpose.

## Commands you will need

```sh
pgrep -af 'cargo|rustc|go build|go test|go vet' | grep -v proc-macro-srv   # must print nothing

RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo test --manifest-path connectors/whatsapp-rust/Cargo.toml
RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --manifest-path connectors/whatsapp-rust/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path connectors/whatsapp-rust/Cargo.toml --check
```

## Scope

In: `connectors/whatsapp-rust/src/{identity.rs (new),addressing.rs (new),testing.rs (new, cfg(test)),main.rs,normalize.rs,chats.rs,disappearing.rs,contacts.rs,names.rs,sender.rs,receipts.rs (if 052 is DONE)}`,
`README.md` (Rust connector section), `plans/README.md`.

Out: the core and the proto (no change needed), the Go connector, media
(051/054/055), settings (056), `.env.local`, `config.local.toml`,
`*.session.json`, `.claude/`, `connectors/instagram/**`, `plans/048-*`.

## Git workflow

Commit directly on `v2`, one commit per step. Do not push. No branches.

## Steps

### Step 0: Baseline

Drift check. Alone: whatsapp-rust tests → record the count.

### Step 1: The identity resolver

1. Rename `normalize::canonical` to `bare_id` everywhere (a mechanical rename;
   doc comment: "The device part stripped, nothing else: the key of the
   connector's own tables. Ids on the wire go through `Identity::canonical`.").
2. New `src/identity.rs`:

```rust
//! One stable identity per WhatsApp peer, as the Go connector's
//! `identity.go` computes it. WhatsApp names one person by a phone JID, a LID
//! and per-device forms of either; the core keys an endpoint (and so a mirror
//! thread) on the chat id, so each form left unmapped would split a chat.

/// Phone-number ↔ LID lookups in the client's own store (no network).
pub trait LidStore: Send + Sync {
    fn lid_for_pn(&self, pn: &Jid) -> impl Future<Output = Option<Jid>> + Send;
    fn pn_for_lid(&self, lid: &Jid) -> impl Future<Output = Option<Jid>> + Send;
}
impl LidStore for Client { /* get_lid_pn_entry; an Err is a miss, logged at debug */ }

/// Canonical ids, with the hits cached for the process (a mapping is stable
/// for the life of a contact); a miss is not cached, as the mapping may be
/// learned later and must then take effect.
#[derive(Default)]
pub struct Identity {
    lids: RwLock<HashMap<String, Jid>>, // phone → LID
    pns: RwLock<HashMap<String, Jid>>,  // LID → phone
}
impl Identity {
    /// Device stripped; a phone JID promoted to its LID when known; any other
    /// server unchanged.
    pub async fn canonical(&self, store: &impl LidStore, jid: &Jid) -> Jid;
    pub async fn canonical_id(&self, store: &impl LidStore, jid: &Jid) -> String;
    /// The other form of a user JID (LID ↔ phone), when known.
    pub async fn alternate(&self, store: &impl LidStore, jid: &Jid) -> Option<Jid>;
}

/// WhatsApp's own hint that a DM's address changed (Go `identityMigration`):
/// the other party's alt address is `sender_alt` on their messages and
/// `recipient_alt` on the account's own (owner Q50); the LID form is the new
/// one. Groups get none: a participant's pair is not a chat migration.
pub fn identity_migration(source: &MessageSource) -> Option<pb::IdentityMigrated>;

/// Every migration a DM implies (Go `migrationsFor`, before the gate): the
/// hint above; the raw chat when it differs from the canonical chat; and the
/// alternate form → the canonical chat. Nothing for a group.
pub fn migrations_for(source: &MessageSource, chat: &Jid, alternate: Option<&Jid>)
    -> Vec<pb::IdentityMigrated>;

/// Drops migrations already reported by this process (Go `migrationGate`).
#[derive(Default)]
pub struct MigrationGate { seen: Mutex<HashSet<(String, String)>> }
impl MigrationGate { pub fn unseen(&self, migrations: Vec<pb::IdentityMigrated>) -> Vec<pb::IdentityMigrated>; }
```

3. New `src/testing.rs` (`#[cfg(test)] pub(crate) mod testing;` in `main.rs`):
   `FakeAccount` implementing `LidStore` (a phone → LID map, with a lookup
   counter) and, from Step 3, `Account`. One fake for every test module.

Tests (`identity.rs`):

1. `a_phone_number_becomes_its_lid` — mapping known → `<lid>@lid`; the
   device is stripped in both cases.
2. `hits_are_cached_misses_are_not` — two `canonical` calls on a mapped
   phone JID → one lookup; two on an unmapped one → two lookups.
3. `groups_lids_and_channels_stay_as_they_are` — `123@g.us`, `9@lid`,
   `1@newsletter` unchanged, with zero lookups.
4. `the_alternate_maps_both_ways`.
5. `a_recipient_alt_makes_the_lid_canonical` — LID addressing mode: old =
   `recipient_alt` (phone), new = chat (LID); PN mode: old = chat, new =
   `recipient_alt` (Go `TestIdentityMigrationTreatsLIDAsCanonical`).
5a. `an_incoming_dm_uses_the_sender_alt` — a message from the other party
   (not from me) with only `sender_alt` set yields the same migration as the
   own-message `recipient_alt` case; with both set on an own message,
   `recipient_alt` wins (owner Q50).
6. `no_hint_for_groups_or_without_an_alt` (Go
   `TestIdentityMigrationIgnoresGroupAndAbsentAlt`).
7. `migrations_cover_the_shift_and_the_alternate` — raw phone chat with a
   known LID → `phone → lid`; a LID chat with a known phone → `phone → lid`
   from the alternate (Go `TestHandleReportsThePhoneFormOfALIDAddressedChat`).
8. `each_migration_is_reported_once` — the gate passes a pair the first time
   only.

**Verify**: whatsapp-rust tests → Step 0 count + 9, green. Mutation: cache
misses too → test 2 fails. Restore.
Commit: `feat(whatsapp-rust): resolve canonical WhatsApp identities like the Go connector`.

### Step 2: Report canonical ids and identity migrations

- `main.rs`: one `Arc<Identity>` and one `Arc<MigrationGate>` (kept inside
  the normalizer), shared by `Normalizer`, `ChatEvents`, the sender (Step 4)
  and `Receipts` (if 052 is DONE).
- `Normalizer::handle`: first, `let chat = identity.canonical(&*ctx.client,
  &info.source.chat)`, the sender likewise, the alternate of the chat; put
  `chat_id`, `sender_id` in `Resolved`, and
  `migrations = gate.unseen(migrations_for(&info.source, &chat, alternate.as_ref()))`.
  Send every migration frame **before** anything else (snapshot, chat
  update, create, edit, delete, reaction), as Go does (`updates.go:110-118`).
  Remove `with_other_forms` and its call.
- `normalize(…)` uses `resolved.chat_id` for every frame's chat and
  `resolved.sender_id` for the create's sender and participant and the
  reaction's sender. The `EPHEMERAL_SETTING` update is reported under
  `resolved.chat_id` only. `first_sighting` uses the canonical chat id.
- `chats.rs`:
  - `group_updates`: canonicalize every member and the actor first
    (`HashMap<bare id, canonical id>`); `group_profile_update` takes that map
    and names `participant` / `actor` from it (names stay keyed by bare id).
  - `picture_updates`: one update under `identity.canonical_id(&update.jid)`.
  - `contact_updates`: still stores names under every form (the contacts
    table); the title update goes under the canonical id only.
  - history sync: `history_updates` (`disappearing.rs`) no longer adds the
    `pn_jid` / `lid_jid` forms; `history_report` keeps using every form to
    find a DM's name, then each update's `chat_id` is canonicalized through
    `Identity` (async, after the blocking decode) — Go canonicalizes each
    conversation the same way (`updates.go:694`).
  - `chat_forms` keeps its body; its doc says it serves the contacts table's
    lookups only, never the wire.
- `receipts.rs` (only if 052 is DONE): chat and observer through
  `identity.canonical_id`.

Tests:

1. `a_message_leads_with_its_migrations` — `normalize` (or the frame-list
   function the handler sends from) with `Resolved.migrations = [phone →
   lid]` → the first frame is `IdentityMigrated`, then the create (Go
   `TestHandleCanonicalizesChatAndEmitsMigrationOnce`).
2. `frames_use_the_canonical_ids` — `Resolved { chat_id: "9@lid", sender_id:
   "9@lid" }` for a message whose raw chat is the phone JID → create chat,
   sender and participant are `9@lid`; a reaction's sender too.
3. `a_timer_setting_is_reported_once` — `EPHEMERAL_SETTING` → one update,
   under the canonical id.
4. Update 045's history-sync test: a conversation with a `pn_jid` → one
   update (the primary id), not two; keep its 1201 → 500/500/201 batching
   assertion.
5. `members_and_actors_are_canonical` — `group_profile_update` with a map
   `1555@s.whatsapp.net → 9@lid` → participant and actor `9@lid`, names
   still found.
6. (052 DONE) `receipt_ids_are_canonical`.

**Verify**: whatsapp-rust tests → green (count: + new tests, 045's test
changed not added). Mutation: send the migrations after the create → test 1
fails; restore `with_other_forms` → test 3 fails. Restore.
Commit: `feat(whatsapp-rust): report canonical ids and identity migrations; chat updates under one form`.

### Step 3: The addressing book

New `src/addressing.rs` (Go `addressing.go`):

```rust
//! The address form (phone JID or LID) each chat is addressed in. Dispatches
//! carry canonical ids, but WhatsApp keys a chat's messages by the chat's own
//! form, so a quote naming a person in the other form does not link to the
//! message it means. This records the form each chat arrives in and
//! translates an address back to it before it goes on the wire.

/// The paired account and WhatsApp's view of it (Go `accountIdentity`).
pub trait Account: Send + Sync {
    fn own_pn(&self) -> Option<Jid>;
    fn own_lid(&self) -> Option<Jid>;
    /// Whether the account moved its DMs to LIDs (the library then sends
    /// every DM to the LID).
    fn lid_migrated(&self) -> impl Future<Output = bool> + Send;
    /// How WhatsApp addresses a group; `None` when the lookup fails.
    fn group_mode(&self, group: &Jid) -> impl Future<Output = Option<AddressingMode>> + Send;
}
impl Account for Client { /* get_pn, get_lid, is_lid_migrated, groups().get_metadata(..).addressing_mode */ }

#[derive(Default)]
pub struct AddressBook { modes: RwLock<HashMap<String, AddressingMode>> }
impl AddressBook {
    /// A group states its form on every message; a DM's form is its raw
    /// chat's server; channels and broadcasts are not recorded.
    pub fn observe(&self, chat_id: &str, source: &MessageSource);
    /// DM: LID when migrated, else recorded, else by server. Group: recorded,
    /// else asked once and remembered; `None` when unknown.
    pub async fn mode(&self, account: &impl Account, chat: &Jid) -> Option<AddressingMode>;
    /// `jid` in `mode` when its counterpart is known, else unchanged.
    pub async fn in_mode(&self, identity: &Identity, store: &impl LidStore, jid: &Jid, mode: Option<AddressingMode>) -> Jid;
    /// The paired account in `mode`: its LID in LID mode when known, else
    /// its phone JID (device stripped).
    pub fn own(&self, account: &impl Account, mode: Option<AddressingMode>) -> Option<Jid>;
}
```

`Normalizer::handle` calls `addresses.observe(&chat_id, &info.source)`
right after canonicalizing (Go `updates.go:104-106`). One `Arc<AddressBook>`
in `main.rs`, shared with the sender. Extend `FakeAccount` with `Account`
(own ids, a migrated flag, group modes, a group-lookup counter, a failing
group).

Tests (`addressing.rs`, Go `addressing_test.go`):

1. `a_group_s_mode_comes_from_its_messages`.
2. `a_group_s_mode_is_asked_once` — no message seen → one lookup, then
   remembered (two `mode` calls, one lookup).
3. `an_unknown_group_mode_keeps_addresses` — lookup fails → `None`;
   `in_mode` returns the address unchanged.
4. `a_direct_chat_s_mode` — migrated → LID whatever was recorded; recorded
   PN → PN; unrecorded `@lid` → LID; unrecorded phone → PN.
5. `known_counterparts_are_translated` — LID in PN mode → phone; phone in
   LID mode → LID; unknown → unchanged; a group → unchanged.
6. `our_own_address_follows_the_mode` — LID mode with and without an own LID;
   PN mode.
7. `the_normalizer_records_each_chat_s_form` — `observe` with a LID group
   source, a PN group source, a `@lid` DM, a phone DM, a channel (not
   recorded) (Go `TestNormalizerReportsTheFormEachChatArrivesIn`).

**Verify**: whatsapp-rust tests → + 7, green. Mutation: drop the
`lid_migrated` branch → test 4 fails. Restore.
Commit: `feat(whatsapp-rust): an addressing book for outbound address forms`.

### Step 4: Quote the real author, in the chat's form

`sender.rs` (if plan 054 is DONE, the sender is generic over 054's
`WaClient` port: make `Account + LidStore` supertraits of `WaClient`,
implement them on `LiveClient` by delegating to its `Client` and on 054's
`FakeClient` by embedding `testing::FakeAccount`, keep 054's typed quoted
body, and put the tests below next to 054's):

- `WhatsAppSender` gains `identity: Arc<Identity>` and `addresses:
  Arc<AddressBook>` (constructor arguments; `main.rs` passes the shared
  ones).
- `target(&self, chat_id) -> Result<(Jid, Option<AddressingMode>), Failure>`:
  parse (permanent on failure, as `parse_chat`), `mode = addresses.mode(..)`,
  `in_mode(chat, mode)`. `send_message`, `edit_message` and
  `delete_message` all use it (Go `sender.go:286-295`).
- Free function, so it is tested with the fake:
  ```rust
  /// Who WhatsApp is told wrote the quoted message (Go `quotedAuthor`).
  /// WhatsApp links a quote by (stanza id, participant), so the wrong author
  /// leaves the reply unlinked. A copy the bridge received here came from
  /// WhatsApp, so the reply context carries its author's own ids (the group
  /// participant, else the chat peer), in canonical form: translate it into
  /// the chat's form. A copy the bridge sent was written by the paired
  /// account, which WhatsApp keys by its LID in a LID-addressed chat.
  pub async fn quoted_author<A: Account + LidStore>(
      account: &A, identity: &Identity, addresses: &AddressBook,
      reply: &pb::ReplyContext, mode: Option<AddressingMode>,
  ) -> Result<String, Failure>;
  ```
  An author that does not parse is used as it is (Go returns it raw). No own
  address → `Failure::permanent("outbound WhatsApp reply requires a paired
  account JID")`.
- `quote_context` builds the context with `build_quote_context(stanza_id,
  participant, &quoted)` (the participant is a string now; the quoted body
  stays the reply text until plan 054 adds typed media stubs). Replace the
  wrong comment.

Tests (`sender.rs`, `#[tokio::test]`, with `FakeAccount`):

1. `a_received_copy_is_quoted_under_its_author` — `authored_by_bridge:
   false`, participant `1555@s.whatsapp.net` with a known LID, LID mode →
   `9@lid`; no participant → the sender; PN mode → the phone form (Go
   `TestQuotedAuthorAttributesTheRealSender`, `TestReplyQuotesAuthorInTheChatsForm`).
2. `a_bridge_copy_is_quoted_under_our_own_address` — LID mode → own LID; PN
   mode → own phone; LID mode without an own LID → own phone.
3. `an_unpaired_account_cannot_quote` → permanent failure.
4. `an_unparseable_author_is_used_as_is`.
5. `a_chat_is_addressed_in_its_form` — `target("9@lid")` with PN mode and a
   known phone → the phone JID; with LID mode → unchanged (Go
   `TestDirectChatIsAddressedInItsForm`, `TestSendDeliversToCanonicalLIDTarget`).

**Verify**: whatsapp-rust tests → + 5, green. Mutation: ignore
`authored_by_bridge` (always own) → test 1 fails. Restore.
Commit: `fix(whatsapp-rust): quote the real author in the chat's address form`.

### Step 5: Channel titles

`names.rs`: `title` handles `Server::Newsletter` with
`self.channel_title(client, &chat)`: `client.newsletter().get_metadata(&chat)`
→ `tagged("C", &metadata.name)`, cached per channel for 5 minutes (a
`Mutex<HashMap<String, (String, Instant)>>` in `ChatTitles`, only non-empty
titles cached, like Go's `ChatTitle`, `names.go:45-60`); a failed lookup →
`None`, logged at debug.

Tests (`names.rs`): `a_channel_title_is_tagged_c` (`tagged("C", "News")` →
`[C] News`; empty → `None`) and a cache test over the pure cache helper
(fresh entry served, an expired one not), mirroring `groups.rs`'s test.

**Verify**: whatsapp-rust tests → + 2, green.
Commit: `feat(whatsapp-rust): name channel mirror topics like the Go connector`.

### Step 6: Docs

- `README.md`, Rust connector section: ids are canonical exactly as the Go
  connector's (a phone JID becomes its LID when known; the core is told
  about both forms), so a connector id can move between the two
  implementations without splitting conversations; replies quote the real
  author; channels are titled `[C]`.

Commit: `docs(whatsapp-rust): identity parity with the Go connector`.

### Step 7: Gate, dev binary, index, and the owner's live test

1. Gate (alone): whatsapp-rust test, clippy, fmt; `git diff --stat 313038d --
   connectors/whatsapp-rust/Cargo.lock` → empty.
2. Dev binary (check `pgrep` first):
   ```sh
   RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 nice -n 19 cargo build --release --manifest-path connectors/whatsapp-rust/Cargo.toml
   install -m 0755 connectors/whatsapp-rust/target/release/wuphf-connector-whatsapp scripts/dev-bin/.bin/wuphf-whatsapp-rust
   ```
3. `plans/README.md`: row 053 DONE with notes (counts, mutations, how the
   dual-form reporting was removed). Commit: `docs(plans): mark plan 053 done`.

**For the owner:**

- **Restart**: the whatsapp-rust connector only (`wwr`).
- **Config**: none. Rules in the core may keep naming a DM by its phone
  number: the migrations link it to the LID form.
- **Live test**:
  1. Before restarting, note the mirror topics of two or three DMs this
     connector bridges. After restarting, have each contact write once → the
     messages land in the **same** topics (no new topic); the connector log
     shows one identity migration per DM at most.
  2. In a mirrored DM, reply from Telegram to a message the **contact** sent
     → on WhatsApp the reply shows the quoted message linked (tap it: it
     jumps to the original). Reply to a message the bridge sent → also
     linked. Repeat in a group.
  3. Change a mirrored group's description → one note in its topic, not two.
  4. Optional parity check: stop the Rust connector; run the Go connector
     with a copy of its config whose `connector_id`, `core_token_env` and
     `supports_disappearing_timers` match the `whatsapp-rust` entry (its own
     session database, paired to the same account); have a contact write in
     a DM → it lands in the same topic. Stop it and start the Rust connector
     again.
  5. If a channel is mirrored, its topic is named `[C] <channel name>`.

## Test plan

| Test | Where | Proves |
|---|---|---|
| canonical, cache, unchanged servers, alternate, migration hint, migrations, gate | whatsapp-rust identity | decisions 1, 2 |
| migrations first, canonical ids in frames, one-form timer / history / members (/ receipts) | whatsapp-rust normalize, chats, disappearing | decisions 1, 2, 6 |
| group mode from messages / asked once / unknown, DM mode, translation, own address, observe | whatsapp-rust addressing | decision 4 |
| received / bridge quotes, unpaired, unparseable, target form | whatsapp-rust sender | decision 3 |
| channel titles | whatsapp-rust names | decision 5 |

## Done criteria

- [ ] Every test above passes; every listed mutation fails a test.
- [ ] No lock file changed; clippy and fmt clean; whatsapp-rust built alone.
- [ ] No chat update is reported under two forms (`grep -n 'chat_forms' connectors/whatsapp-rust/src`
      shows only the contacts-table uses).
- [ ] Dev binary installed; row 053 DONE with notes and the owner's steps.

## STOP conditions

- The drift check fails, or a "must still hold" fact is false.
- `get_lid_pn_entry`, `is_lid_migrated`, `GroupMetadata.addressing_mode`,
  `MessageSource.recipient_alt` / `addressing_mode` or
  `newsletter().get_metadata` is missing or different at `5ca80c5c`.
- The core does not link the forms on `IdentityMigrated` (a profile written
  under the phone form is not found for the LID conversation in a core
  test) — that would make removing the dual-form reports lose data.
- The live test shows a new mirror topic for an existing DM (the migration
  did not bind the old endpoint): stop and report the connector log and the
  two chat ids.
- Any test that existed at Step 0 starts failing for a reason this plan does
  not explain (045's history-sync test changes on purpose in Step 2).

## Maintenance notes

- **Ids on the wire go through `Identity`; `bare_id` is for the connector's
  own tables.** A new frame field holding a person or chat must use
  `Identity::canonical_id`, or the two connectors drift apart again.
- **`chat_forms` is for local lookups only.** Reporting a chat update under
  more than one form now posts duplicate notes in mirror topics.
- **A mapping learned late** (the LID of a contact first seen by phone)
  changes that contact's canonical id; the next message reports the shift as
  a migration, as in Go. Cached hits never change for the process.
- The library already addresses DMs in the account's namespace; if a future
  whatsapp-rust stops doing that, `target()` keeps sends correct.
- **Channel titles are cached for 5 minutes**, like Go's titles; a renamed
  channel is picked up after that.
