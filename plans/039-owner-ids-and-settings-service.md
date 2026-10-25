# Plan 039: Owner ids per connector, and the core serving its settings

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-cli crates/wuphf-runtime/src/observer_service.rs crates/wuphf-runtime/src/grpc.rs crates/wuphf-runtime/src/connector.rs crates/wuphf-storage/src/lib.rs crates/wuphf-settings migrations config/example.toml Cargo.toml`
> Plans 034–038 are expected to have changed these files (034 adds connector
> tokens; 035 moves config loading onto figment and a `WuphfSchema`; 036 adds
> the settings table; 037 makes the core read a live snapshot; 038 turns
> `rules` into a map). What must still hold: there is one `serve()` in
> `crates/wuphf-cli/src/main.rs` building one `tonic::transport::Server`;
> `[[connectors]]` entries are deserialized into one struct per connector;
> `bearer_auth` exists in `crates/wuphf-runtime/src/observer_service.rs`; the
> core's settings live in a `botconf::SettingsStore<WuphfSchema>` (035).
> Otherwise STOP.

## Status

- **Priority**: P2
- **Effort**: M–L
- **Risk**: MED — adds a network surface that can change routing; the
  authorization rule is the whole point and must be tested
- **Depends on**: 034 (connector tokens), 035 (figment + `WuphfSchema`),
  036 (sqlx settings storage), 037 (live snapshot), 038 (rules keyed by id,
  chats as `connector:chat#thread` strings); botconf plans 003 (`Kind::Ref`
  domains, string actors), 004 (`botconf-panel` and its `TokenStore`) and 005
  (`botconf.v1.Settings` + `botconf-grpc`) landed and tagged
- **Category**: direction
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

Plans 035–037 make the core's configuration a runtime-editable botconf store,
but nothing outside the process can reach it yet. This plan mounts botconf's
generic gRPC settings service in the core, so the TUI, the `wuphf settings`
CLI (042) and the connectors' panels (041) can read and change settings.

Changing settings changes routing, so the service must only obey the owner.
Today the core has no notion of who the owner is on any platform: the only
owner signal is the `OWNER_AUTHOR` label stamped on core-originated sends
(`crates/wuphf-model/src/dispatch.rs:203`). This plan adds that root of trust:
a file-only list of the owner's platform user ids per connector, checked by
the core on every settings request a connector makes on someone's behalf.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The settings store lives in the core. Connectors never decide who may change settings; they pass along who acted, and the **core** authorizes. |
| 2 | Each `[[connectors]]` entry gains `owner_ids`: the platform user ids of the owner on that connector, as strings. They are **file-only**: never runtime keys, like the assistant bot's `owner_id`. |
| 3 | A connector authenticates to the settings service with **its own connector token** from plan 034. A request is allowed only when the acting platform user id is in **that** connector's `owner_ids`. A connector cannot act for another connector's owners. |
| 4 | The service is botconf's generic `botconf.v1.Settings`, served by the `botconf-grpc` crate. wuphf only mounts it and supplies the authorization hook and the name lookup. |
| 5 | wuphf registers three `Kind::Ref` domains: `wuphf.chat` (a chat on a connector, in 038's `connector:chat#thread` form), `wuphf.connector` (a configured connector id) and `wuphf.rule` (a rule id). |
| 6 | Every chat a setting names is a `wuphf.chat` ref, so the panel shows its **name**: `notify` and every chat inside a rule. Names come from what the core has already stored (observed chat titles, conversation titles); a chat with no known name shows its raw id. Adding a chat offers a picker of chats the core has seen (owner, 2026-10-25). |
| 7 | Panel buttons use botconf-panel's hybrid tokens: a stateless token when the action fits the platform's limit and does not pick from a dynamic list, otherwise a short handle derived from the action and stored in SQLite for 30 days, renewed whenever a redraw issues it again. wuphf provides the store (owner, 2026-10-25). |

### Decision made by this plan (confirmed by the owner 2026-10-25)

| # | Decision | Why |
|---|---|---|
| F1 | The **observer token** also grants settings access, as the owner. A request carrying it needs no actor id. | The TUI already holds the token that can send messages as the owner (`observer_service.rs`, `with_sender`); settings are the same trust level. With no observer token configured, settings over the observer credential are refused (not allowed-all), unlike the observer itself. |

## Current state

- `crates/wuphf-cli/src/main.rs:91-99` — one struct per connector entry:

  ```rust
  #[derive(Deserialize)]
  struct ConnectorConfiguration {
      id: String,
      platform: String,
      #[serde(default)]
      supports_threads: bool,
      #[serde(default)]
      supports_topic_create: bool,
  }
  ```
  Plan 034 adds a token field here (`token_env`, or whatever it landed as);
  plan 035 may have moved the struct into a config module. Use its landed
  location.

- `crates/wuphf-cli/src/main.rs:123-199` — `serve()` builds every service and
  one `Server::builder()`. The observer is mounted with
  `ObserverTransportServer::with_interceptor(…, bearer_auth(configuration.observer_token.clone()))`.

- `crates/wuphf-runtime/src/observer_service.rs:264-285` — `bearer_auth(token:
  Option<String>)`: `None` allows every request; `Some(t)` requires
  `authorization: Bearer <t>`.

- `crates/wuphf-runtime/src/connector.rs:96-110` — the hub only checks that a
  registration equals the configured `[[connectors]]` entry; plan 034 adds the
  per-connector token on `ConnectorTransport` and `MediaTransport`.

- Where chat names are stored today (the name sources for `wuphf.chat`):
  - **Observed chats.** `observed_chats (connector_id, chat_id,
    last_message_ts, payload)` with primary key `(connector_id, chat_id)`
    (`migrations/0008_observation_log.sql:18-24`). The payload is an
    `ObservedChat` (`crates/wuphf-model/src/observation.rs:95-102`:
    `connector_id`, `platform`, `chat_id`, `title`, `last_message_ts`,
    `last_message_preview`). `log_observed` refreshes `title` whenever an
    inbound message carries `chat_title` and keeps the old one otherwise
    (`crates/wuphf-storage/src/lib.rs:190-202`, `sqlite.rs:618-660`). This
    covers every chat anyone has written in, routed or not. The only reader
    today is `ObservationStore::list_observed_chats()` (`lib.rs:236`,
    `sqlite.rs:841-848`): every chat, most recent first — fine for a picker,
    wasteful for one name.
  - **Conversation titles.** `Conversation.title` (`routing.rs:322-336`) is
    the source chat's last seen title, set by `set_conversation_title`
    (`lib.rs:99`). A conversation lists its endpoints, and a mirror thread is
    an endpoint with a `thread_id`, so a thread's name is the title of the
    conversation it belongs to: `TopologyStore::resolve_endpoint(connector,
    address)` (`lib.rs:53-57`) then `conversation_for_endpoint` (`lib.rs:81`).
  - **Other address forms.** A rule may name a WhatsApp chat by its phone JID
    while the observation log keyed it under the LID. `linked_addresses`
    (`lib.rs:67-71`) returns the directly linked forms.
- `crates/wuphf-settings` (035, 038): the config-facing `ChatTarget` newtype
  and `RuleEntry`/`RuleEntryKind` with `source`, `destination`,
  `source_connector`, `only_chats`, `except_chats` and `priority`;
  `ChatRef`'s `Display`/`FromStr` in `wuphf-model` (038).

- `crates/wuphf-tui/src/main.rs:30-37` — the TUI takes `--server` and
  `--token` (the observer token).

- botconf (landed by its plans 003 and 005; **check the tagged API**). This
  plan assumes these shapes; STOP if they differ materially:

  ```rust
  // botconf (plan 003)
  Kind::Ref { domain: &'static str, many: bool }

  // botconf-grpc (plan 005)
  pub struct SettingsService<S: Schema> { /* … */ }
  impl<S: Schema> SettingsService<S> {
      pub fn new(store: Arc<SettingsStore<S>>, auth: impl Authorize) -> Self;
      pub fn with_names(self, names: impl RefNames) -> Self;
  }
  pub fn server<S: Schema>(service: SettingsService<S>) -> SettingsServer<…>;

  /// Decides each request. `actor` is the platform user id a front end acts
  /// for (None for direct callers such as a CLI).
  #[tonic::async_trait]
  pub trait Authorize: Send + Sync + 'static {
      async fn authorize(&self, metadata: &MetadataMap, actor: Option<&str>)
          -> Result<Principal, Status>;
  }
  /// Who a request runs as; recorded as the change's author. botconf plan
  /// 003 makes the stored actor an opaque string.
  pub struct Principal { pub label: String, pub by: Option<String> }

  #[tonic::async_trait]
  pub trait RefNames: Send + Sync + 'static {
      async fn name(&self, domain: &str, id: &str) -> Option<String>;
      /// Values to offer when adding to a ref setting, best first, with
      /// their names; `query` is what the user typed, if anything.
      async fn candidates(&self, domain: &str, query: Option<&str>, limit: usize)
          -> Vec<(String, Option<String>)>;
  }

  // botconf-panel (plan 004, "Tokens"): the store behind handles, the
  // buttons whose stateless token would be too long. A handle is `~` plus
  // 10 base62 characters hashed from the encoded action (FNV-1a), so a
  // redraw renews the same row; the engine retries the next handle on a
  // collision and purges expired rows itself, at most hourly.
  pub trait TokenStore: Send + Sync + 'static {
      /// Replaces `key` if it holds the same action or an expired one
      /// (renewing the expiry); `false`, changing nothing, if it holds a
      /// different unexpired action.
      fn put<'a>(&'a self, key: &'a str, action: &'a str, now_ms: i64, expires_at_ms: i64)
          -> BoxFuture<'a, Result<bool, StorageError>>;
      fn get<'a>(&'a self, key: &'a str, now_ms: i64)
          -> BoxFuture<'a, Result<Option<String>, StorageError>>;
      fn purge_expired(&self, now_ms: i64) -> BoxFuture<'_, Result<u64, StorageError>>;
  }

  impl<S: Schema> Panel<S> {
      pub fn new(store: Arc<SettingsStore<S>>) -> Self; // MemoryTokenStore, 30-day TTL
      pub fn token_store(self, tokens: impl TokenStore) -> Self;
  }
  // A handle the store no longer has: PanelError::Stale { notice, screen },
  // which botconf-grpc's Act returns as `stale = true` (plan 005).
  ```

  The panel renders a ref as `Name (id)` when `name` returns `Some`, and the
  raw id otherwise (botconf 004); wuphf only supplies names.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Build | `cargo build --workspace --all-targets` | exit 0 |
| CLI tests | `cargo test -p wuphf-cli` | exit 0 |
| Runtime tests | `cargo test -p wuphf-runtime` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline + new tests (147 at `643829b`, more after 034–038) |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |

`protoc` must be on PATH (`nix develop`); botconf-grpc compiles its own proto.

## Scope

**In scope**:
- the connector configuration struct (`owner_ids`) and its validation
- `crates/wuphf-runtime/src/settings_auth.rs` (create): the `Authorize` and
  `RefNames` implementations
- `crates/wuphf-storage`: a narrow `ObservationStore::observed_chat` reader;
  a `PanelTokens` role trait with its sqlx and in-memory implementations;
  `migrations/0017_panel_tokens.sql` (create);
  `crates/wuphf-storage/src/{lib.rs,sqlite.rs,memory.rs,tests.rs}`
- `crates/wuphf-runtime/src/lib.rs` (module line), `crates/wuphf-runtime/Cargo.toml`
- `crates/wuphf-cli/src/main.rs` (mount the service), `crates/wuphf-cli/Cargo.toml`
- the `WuphfSchema` runtime settings (035's module): switch connector and rule
  id fields to `Kind::Ref`
- root `Cargo.toml` (`botconf-grpc` git dependency, pinned to the same tag as
  `botconf`), `Cargo.lock`
- `config/example.toml`, `README.md`, `docs/architecture.md`,
  `docs/connector-protocol.md`

**Out of scope** (do NOT touch):
- `ConnectorTransport` and connector code — panels are 041.
- Connector-declared settings — 040.
- The `wuphf settings` CLI — 042.
- Any change to routing behaviour.

## Git workflow

- Commit on the current branch (the owner's rule: no new branches unless
  asked).
- Commits:
  1. `feat(config): add owner_ids per connector`
  2. `feat(storage): look up one observed chat; keep panel tokens`
  3. `feat(runtime): authorize settings requests and name chats`
  4. `feat(cli): serve the botconf settings service`
  5. `feat(settings): chats and connectors as references`
  6. `docs: describe the settings service and owner ids`
- Do NOT push.

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ record the number (147 at `643829b`, more once 034–038 landed).

### Step 1: Add `owner_ids` to each connector entry

Add to the connector configuration struct:

```rust
/// Platform user ids of the owner on this connector, as the connector
/// reports actors (e.g. "704338780" for a Telegram user). Requests a
/// connector makes on behalf of anyone else are refused. File-only: never a
/// runtime setting, since it decides who may change the others.
#[serde(default)]
owner_ids: Vec<String>,
```

Validation (in `WuphfSchema::derive`, next to 035's other checks): every id is
non-empty after trimming, and no id repeats within a connector. Carry the
list into the derived data as `HashMap<ConnectorId, HashSet<String>>` named
`owners`, so the authorizer reads it from the current snapshot.

Make sure `owner_ids` is **not** in the runtime key list (035's
`CORE_SETTINGS`, or whatever it is called).

**Verify**: a CLI test `connector_owner_ids_parse` parses a config with
`owner_ids = ["704338780"]` on one connector and none on another, and asserts
both; `owner_ids_must_not_be_blank` asserts `owner_ids = [" "]` is rejected.
Then try changing `connectors` or an `owner_ids` key through the store
(`store.set(...)`) in a unit test, `owner_ids_are_not_runtime_keys`, and
assert `SettingsError::UnknownKey`.

Commit 1.

### Step 2: Storage — one observed chat, and panel tokens

Two additions to `crates/wuphf-storage`, each on both backends and covered by
the dual-backend suite in `tests.rs`.

**One observed chat.** Add to `ObservationStore`:

```rust
/// The observation log's summary of one chat, if it has seen the chat.
async fn observed_chat(
    &self,
    connector_id: &ConnectorId,
    chat_id: &str,
) -> StorageResult<Option<ObservedChat>>;
```

SQLite: `SELECT payload FROM observed_chats WHERE connector_id = ? AND
chat_id = ?` (the primary key). Memory: a lookup in its map. Test
`observed_chat_returns_the_latest_title`: log two messages in one chat, the
first with title "Old", the second with "New", then one with no title;
`observed_chat` returns "New"; an unknown chat returns `None`.

**Panel tokens.** botconf-panel's handles (decision 7; botconf plan 004,
"Tokens") need a store that survives restarts, or every button carrying a
handle goes stale when the core restarts. A handle is `~` plus 10 base62
characters derived from a hash of the encoded action, so redrawing a screen
renews the same row instead of adding one. Add
`migrations/0017_panel_tokens.sql` (the next free number after 036's `0015`
and 038's `0016`; check `ls migrations/`), mirroring the table botconf's
`create_tokens_table` makes for sea-orm hosts:

```sql
-- Settings-panel button handles: actions too long for a platform's button
-- data, stored under a short key derived from the action (botconf-panel's
-- hybrid tokens). Rows expire after 30 days unless a redraw renews them;
-- botconf-panel purges expired rows itself, at most hourly.
CREATE TABLE panel_tokens (
    key TEXT PRIMARY KEY NOT NULL,
    action TEXT NOT NULL,
    expires_at INTEGER NOT NULL -- milliseconds since the Unix epoch
);
CREATE INDEX panel_tokens_expiry_idx ON panel_tokens (expires_at);
```

Add a role trait `PanelTokens` in `lib.rs` with the exact semantics of
botconf-panel's `TokenStore`, implemented for `SqliteStorage` and
`InMemoryStorage`:

- `put(key, action, now_ms, expires_at_ms) -> bool` — store `action` under
  `key` until `expires_at_ms`. If `key` already holds **the same action**, or
  any action that expired before `now_ms`, replace it (renewing the expiry)
  and return `true`. If it holds a **different unexpired** action (a hash
  collision), change nothing and return `false`; the engine then tries its
  next handle. In SQLite this is one statement:

  ```sql
  INSERT INTO panel_tokens (key, action, expires_at) VALUES (?1, ?2, ?4)
  ON CONFLICT(key) DO UPDATE SET action = excluded.action, expires_at = excluded.expires_at
  WHERE panel_tokens.action = excluded.action OR panel_tokens.expires_at < ?3
  ```

  and `true` exactly when a row was written (`rows_affected() == 1`).
- `get(key, now_ms) -> Option<String>` — the action, unless the key is absent
  or its row expired at `now_ms` (`expires_at >= now_ms` counts as live).
- `purge_expired(now_ms) -> u64` — delete the rows with `expires_at < now_ms`
  and return how many.

Like 036's `SettingsOverrides`, keep `PanelTokens` out of the `Storage` sum
trait: the core's routing never reads it. Then implement botconf-panel's
`TokenStore` for an adapter over `Arc<impl PanelTokens>` in `wuphf-settings`
(next to 036's `botconf::Storage` adapter), mapping errors into botconf's
`StorageError`. No timer is needed: the engine purges opportunistically (at
most once an hour, on a `put`).

Why here and not in 036: the table only makes sense once botconf-panel
exists (botconf plan 004), and 036 must stay landable on botconf plan 001
alone.

Tests (dual backend):

- `panel_tokens_round_trip` — `put` then `get` returns the action.
- `putting_the_same_action_renews_it` — `put` twice with a later expiry;
  both return `true`, and `get` just before the second expiry still finds it.
- `a_live_collision_is_refused` — `put(k, "a")`, then `put(k, "b")` before
  `a` expires returns `false` and `get(k)` is still `"a"`; after `a` expired,
  `put(k, "b")` returns `true`.
- `expired_panel_tokens_are_not_returned`.
- `purge_removes_only_expired_tokens` — returns the count and leaves live rows.

Storage migration test: none needed (a new table, no data rewrite); the
existing `migration_tests.rs` cases must still pass.

**Verify**: `cargo test -p wuphf-storage` → all pass.

Commit 2.

### Step 3: The authorization hook

Create `crates/wuphf-runtime/src/settings_auth.rs` with a `//!` comment
stating decisions 1–3 and F1. Implement botconf-grpc's `Authorize`:

```rust
pub struct WuphfAuthorizer<S: Schema> {
    store: Arc<SettingsStore<S>>,
    /// Plan 034's token → connector lookup.
    connectors: ConnectorTokens,
    /// The observer token; `None` refuses the observer credential (F1).
    observer_token: Option<String>,
}
```

`authorize(metadata, actor)`:

1. Read `authorization: Bearer <token>`. Missing → `unauthenticated`.
2. If `observer_token` is `Some` and equals the token → `Principal { label:
   "observer", by: Some("observer".into()) }` (actor ignored).
3. Else look the token up in plan 034's connector tokens. Unknown →
   `unauthenticated`.
4. The token named connector `C`. No `actor` → `permission_denied("a
   connector must say who is acting")`. `actor` not in the current snapshot's
   `owners[C]` → `permission_denied("not allowed")`. Otherwise →
   `Principal { label: format!("{C}:{actor}"), by: Some(format!("{C}:{actor}")) }`,
   so the settings table's `updated_by` records which connector and which
   platform user made the change.

Read `owners` from `store.current()` on every call, so 035's reload keeps
working (but `owner_ids` itself only changes on reload of the file).

Compare tokens the same way `bearer_auth` does (constant-time comparison is a
listed follow-up in 032's maintenance notes; keep it consistent).

**Verify**: `cargo test -p wuphf-runtime settings_auth` → the authorization
cases in the test plan pass.

### Step 4: Names and candidates

In the same module implement `RefNames` as `WuphfNames`, holding the storage
and the settings store:

- `wuphf.connector` → `"<id> (<platform>)"` from the snapshot's connector
  list, e.g. `"telegram-main (telegram)"`; unknown ids → `None`.
- `wuphf.rule` → the rule id (038 keys rules by id; nothing nicer exists).
- `wuphf.chat` → parse the id with `ChatRef::from_str` (038); unparsable →
  `None`. Then:
  1. the chat's name: `observed_chat(connector, chat_id)`'s `title`; if none,
     try each of `linked_addresses(connector, address-without-thread)` the
     same way (so a rule written with a phone JID finds the title logged
     under the LID); if still none, the `title` of a conversation whose
     source is this chat (`resolve_endpoint` on the address without thread,
     then `conversation_for_endpoint`).
  2. with a `thread_id`: the thread's name is the `title` of the
     conversation that endpoint belongs to (`resolve_endpoint(connector,
     full address)` then `conversation_for_endpoint`) — a mirror thread is
     named after the chat it mirrors. Return `"<chat name> › <thread name>"`
     when both are known, whichever one is known otherwise.
  3. nothing known → `None` (the panel shows the raw id).

  Cache nothing in this plan: each lookup is one or two primary-key or
  indexed queries, and panels render rarely.

`candidates(domain, query, limit)`:

- `wuphf.chat` → `list_observed_chats()` (most recent first), each as
  `ChatRef { connector_id, address: ChatAddress::new(chat_id, None) }`
  rendered with `to_string()`, named by its `title`; filtered by `query`
  (case-insensitive substring of the title or the id) when given; at most
  `limit`. Chats on connectors that are no longer configured are skipped.
- `wuphf.connector` → the configured connectors, in config order.
- `wuphf.rule` → the configured rule ids, sorted.

**Verify**: these unit tests, on `InMemoryStorage`:

- `chat_names_come_from_the_observation_log` — log a message with chat title
  "Family" in `whatsapp-main:120363@g.us`; `name("wuphf.chat",
  "whatsapp-main:120363@g.us")` is `"Family"`.
- `chat_names_follow_linked_addresses` — title logged under `98765@lid`, a
  link to `12345@s.whatsapp.net` recorded; the name of
  `whatsapp-main:12345@s.whatsapp.net` is the LID's title.
- `thread_names_come_from_their_conversation` — a conversation titled
  "Family" with a mirror endpoint `telegram-main:telegram:channel:1#7`; the
  name of that ref includes "Family".
- `unknown_chats_have_no_name` — `None` for an unseen chat and for an
  unparsable id.
- `chat_candidates_are_recent_and_filtered` — three observed chats; the
  candidates are most recent first, `query = "fam"` keeps only "Family",
  `limit = 1` returns one.

Commit 3.

### Step 5: Mount the service

Add `botconf-grpc` and `botconf-panel` to the root `[workspace.dependencies]`
(git dependency on `https://github.com/propheci/botconf`, same tag as
`botconf`) and to `wuphf-cli`. In `serve()`, after the settings store is
loaded (035):

```rust
let panel = botconf_panel::Panel::new(Arc::clone(&settings))
    .token_store(PanelTokenStore::new(Arc::clone(&storage)));
let settings_service = botconf_grpc::SettingsService::new(
    Arc::clone(&settings),
    WuphfAuthorizer::new(Arc::clone(&settings), connector_tokens.clone(), configuration.observer_token.clone()),
)
.with_panel(panel)
.with_names(WuphfNames::new(Arc::clone(&storage), Arc::clone(&settings)));
// …
.add_service(botconf_grpc::server(settings_service))
```

(Use botconf's landed names for the builder calls; the concepts — an
authorizer, a name lookup, and a panel built with a persistent token store —
must all be pluggable.) No purge task: the panel purges expired handles on
its own, at most hourly.

A press on a handle the store no longer has (expired, or from before a
database reset) comes back from `Act` as `stale = true` with a notice and a
redrawn screen (botconf 005); front ends show the notice and redraw. Nothing
in wuphf handles it.

No `bearer_auth` interceptor on this service: `Authorize` does it, because it
needs to know which token was presented.

When `observer_token` is unset, log once:
`tracing::warn!("settings service: no observer token set; only connectors acting for their owners can change settings")`.

**Verify**: `cargo build --workspace --all-targets` → exit 0. End-to-end test
`settings_service_end_to_end` in `crates/wuphf-cli` (or `wuphf-runtime`'s
`tests.rs`, following its TCP harness at `tests.rs:486-500`): start the
service on `127.0.0.1:0` with one connector token and one owner id; with the
connector token and actor = the owner, `Set` a runtime key and see it in
`store.current()` with `updated_by = "<connector>:<owner id>"` in the
settings table; with actor = someone else, get `PERMISSION_DENIED` and no
change. A second test, `panel_handles_survive_a_restart`: render a screen
whose button needs a handle (a rule entry with a long id), drop the service,
start a new one over the same SQLite file, press the handle → not stale.

Commit 4.

### Step 6: Refs in the runtime settings

In 035's runtime settings list (as reshaped by 037 and 038), change the
kinds:

- `notify` → `Kind::Ref { domain: "wuphf.chat", many: true }`.
- Each rule entry (`rules`, per-entry) → `Kind::Form(&RULE_FORM)` instead of
  `Kind::Json`, with these fields (all optional so they can be cleared;
  `priority` as a number):

  | Field | Kind | Group |
  |---|---|---|
  | `kind` | `OneOf` fixed `one-to-one` / `many-to-one`, not custom | — |
  | `priority` | `Number { min: -1000, max: 1000, unit: "", suggestions: &[-1, 0, 1] }` | — |
  | `source` | `Ref { domain: "wuphf.chat", many: false }` (one-to-one) | — |
  | `source_connector` | `Ref { domain: "wuphf.connector", many: false }` (many-to-one) | — |
  | `destination` | `Ref { domain: "wuphf.chat", many: false }` | — |
  | `only_chats` | `Ref { domain: "wuphf.chat", many: true }` | `filter` |
  | `except_chats` | `Ref { domain: "wuphf.chat", many: true }` | `filter` |

  The `filter` group makes setting one list clear the other, matching 038's
  "not both" rule. Fields that do not apply to the chosen `kind` stay unset;
  the form's `initial` is `{"kind": "many-to-one"}`. `derive` still
  validates the whole entry, so a half-filled form is rejected with 038's
  messages.
- Any setting that lists rule ids (none today; skip if absent) →
  `Kind::Ref { domain: "wuphf.rule", many: true }`.

Tests in `wuphf-settings`:

- `ref_domains_are_wuphf_ones` — walk the catalog and assert every
  `Kind::Ref` domain (including inside forms) starts with `wuphf.`.
- `notify_and_rule_chats_are_chat_refs` — `notify`, and the `source`,
  `destination`, `only_chats` and `except_chats` fields of the rule form, are
  `wuphf.chat` refs.
- `a_rule_can_be_built_field_by_field` — through the store, set
  `rules.new` to the form's initial value plus `source_connector` and
  `destination` (as the panel would, one field at a time): the rule routes;
  then set `only_chats`, then `except_chats`, and assert the stored entry
  holds only `except_chats` (the group cleared the other).

Commit 5.

### Step 7: Document

- `config/example.toml`: under the first `[[connectors]]`, add a commented
  `# owner_ids = ["704338780"]` with one line explaining it (platform user ids
  of the owner on this connector; who may change settings from this
  platform; never editable at runtime).
- `docs/architecture.md`: a "Settings" paragraph — the store in the core,
  the service, decisions 1–3, 6, 7 and F1, and where chat names come from.
- `docs/connector-protocol.md`: a "Settings" section — which token to send,
  the `actor` rule, the status codes (`UNAUTHENTICATED`, `PERMISSION_DENIED`),
  and that connectors must never mirror a settings command.
- `README.md`: one line under configuration pointing at the docs.

**Verify**: `grep -c owner_ids config/example.toml docs/connector-protocol.md` → each ≥ 1.

Commit 6.

### Step 8: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0; total = Step 0 + new tests.

## Test plan

In `crates/wuphf-runtime/src/settings_auth.rs` (unit, `#[tokio::test]`):

1. `missing_token_is_unauthenticated`
2. `unknown_token_is_unauthenticated`
3. `observer_token_acts_as_owner` (actor ignored)
4. `observer_credential_refused_when_no_observer_token` — no observer token
   configured, a request presenting an empty bearer is refused.
5. `connector_without_actor_is_denied`
6. `connector_acting_for_its_owner_is_allowed`
7. `connector_acting_for_a_stranger_is_denied`
8. `connector_cannot_act_for_another_connectors_owner` — owner of `B`, token
   of `A` → denied.
9. `owner_ids_follow_reload` — change the file's `owner_ids`, `reload()`, the
   new id is allowed and the old one denied.
10. `chat_names_come_from_the_observation_log`
11. `chat_names_follow_linked_addresses`
12. `thread_names_come_from_their_conversation`
13. `unknown_chats_have_no_name`
14. `chat_candidates_are_recent_and_filtered`

In `wuphf-storage` (both backends): `observed_chat_returns_the_latest_title`,
`panel_tokens_round_trip`, `putting_the_same_action_renews_it`,
`a_live_collision_is_refused`, `expired_panel_tokens_are_not_returned`,
`purge_removes_only_expired_tokens`.

In `wuphf-settings`: `ref_domains_are_wuphf_ones`,
`notify_and_rule_chats_are_chat_refs`, `a_rule_can_be_built_field_by_field`.

In `wuphf-cli`: `connector_owner_ids_parse`, `owner_ids_must_not_be_blank`,
`owner_ids_are_not_runtime_keys`, `settings_service_end_to_end`,
`panel_handles_survive_a_restart`.

## Done criteria

- [ ] `cargo test --workspace` exits 0; the cases above exist by name
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings;
      `cargo fmt --check` exits 0
- [ ] `owner_ids` cannot be changed through the settings store
- [ ] `git diff --stat 643829b..HEAD` for this plan's commits lists only
      in-scope paths
- [ ] `plans/README.md` status row updated

## STOP conditions

- botconf-grpc's authorization hook cannot see which bearer token was
  presented, or gives no way to pass an actor id — the design needs both.
- botconf's tagged API differs materially from the shapes in "Current state"
  (names may differ; the concepts must exist). In particular: no way to
  supply names for a ref domain, no way to offer candidates when adding to a
  ref setting, refs not allowed as form fields, or no pluggable
  `TokenStore`.
- Plan 034 did not land per-connector tokens, or tokens are not resolvable to
  a connector id from request metadata.
- Plan 038 did not land the `connector:chat#thread` string form or the flat
  `only_chats`/`except_chats` rule fields.
- Mounting the service needs a change to `ConnectorTransport` or to routing.

## Maintenance notes

- `owner_ids` is per connector, not per platform: two Telegram connectors
  (two accounts) may have different owners.
- For a Telegram **user** session, the owner types from the session's own
  account, so that account's user id must be in `owner_ids` (041 sends it as
  the actor).
- Chat names are only as good as what connectors report: a chat nobody has
  written in since the observation log started has no title. If that proves
  annoying, a later plan can ask the connector for a chat's title on demand
  (a connector RPC), keeping the core authoritative for the lookup.
- Constant-time token comparison, TLS and per-key permissions (e.g. a
  co-owner who may only edit rendering) are deferred.
