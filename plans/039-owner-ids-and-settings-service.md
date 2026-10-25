# Plan 039: Owner ids per connector, and the core serving its settings

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-cli crates/wuphf-runtime/src/observer_service.rs crates/wuphf-runtime/src/grpc.rs crates/wuphf-runtime/src/connector.rs crates/wuphf-storage/src/lib.rs config/example.toml Cargo.toml`
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
- **Effort**: M
- **Risk**: MED — adds a network surface that can change routing; the
  authorization rule is the whole point and must be tested
- **Depends on**: 034 (connector tokens), 035 (figment + `WuphfSchema`),
  036 (sqlx settings storage), 037 (live snapshot); botconf plans 003
  (`Kind::Ref` domains) and 005 (`botconf.v1.Settings` + `botconf-grpc`)
  landed and tagged
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
| 5 | wuphf registers three `Kind::Ref` domains: `wuphf.chat` (a chat on a connector), `wuphf.connector` (a configured connector id) and `wuphf.rule` (a rule id). Chat names come from the observation log. |

### Decision made by this plan (flagged for the owner)

| # | Decision | Why |
|---|---|---|
| F1 | The **observer token** also grants settings access, as the owner. A request carrying it needs no actor id. | The TUI already holds the token that can send messages as the owner (`observer_service.rs`, `with_sender`); settings are the same trust level. With no observer token configured, settings over the observer credential are refused (not allowed-all), unlike the observer itself. |

If the owner rejects F1, drop Step 5's observer branch and the TUI and CLI
reach settings only through a connector-style credential.

## Open question for the owner (answer before Step 6)

botconf plan 003 makes `Kind::Ref` values opaque **strings**. wuphf's chat
references are structs (`ChatRef { connector_id, address: { chat_id,
thread_id } }`) in `notify` and inside rules. Either:

- **(a)** add a canonical string form for `ChatRef`
  (`<connector_id>/<chat_id>` or `<connector_id>/<chat_id>#<thread_id>`) with
  `Display`/`FromStr`, and make the settings that hold chats accept that form
  (a config format change for `notify`, and for 038's rule fields); or
- **(b)** keep the structs, use `Kind::Ref` only for `wuphf.connector` and
  `wuphf.rule` (plain strings already), and edit chat fields as `Kind::Json`
  until botconf can carry structured ref values.

Recommendation: **(b)** for this plan — it changes no config format — and a
follow-up once botconf supports structured refs. Step 6 is written for (b);
if the owner picks (a), STOP and re-plan Step 6.

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

- `crates/wuphf-storage/src/lib.rs:236` — `ObservationStore::list_observed_chats()
  -> Vec<ObservedChat>`; `ObservedChat` (`crates/wuphf-model/src/observation.rs:95-102`)
  carries `connector_id`, `chat_id` and `title`. This is the name source for
  `wuphf.chat`.

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
  /// Who a request runs as; recorded as the change's author.
  pub struct Principal { pub label: String, pub by: Option<i64> }

  #[tonic::async_trait]
  pub trait RefNames: Send + Sync + 'static {
      async fn name(&self, domain: &str, id: &str) -> Option<String>;
  }
  ```

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
  2. `feat(runtime): authorize settings requests by connector owner`
  3. `feat(cli): serve the botconf settings service`
  4. `docs: describe the settings service and owner ids`
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

### Step 2: The authorization hook

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
   "observer", by: None }` (actor ignored).
3. Else look the token up in plan 034's connector tokens. Unknown →
   `unauthenticated`.
4. The token named connector `C`. No `actor` → `permission_denied("a
   connector must say who is acting")`. `actor` not in the current snapshot's
   `owners[C]` → `permission_denied("not allowed")`. Otherwise →
   `Principal { label: format!("{C}:{actor}"), by: None }`.

Read `owners` from `store.current()` on every call, so 035's reload keeps
working (but `owner_ids` itself only changes on reload of the file).

Compare tokens the same way `bearer_auth` does (constant-time comparison is a
listed follow-up in 032's maintenance notes; keep it consistent).

**Verify**: `cargo test -p wuphf-runtime settings_auth` → the cases in the
test plan pass.

### Step 3: The name lookup

In the same module implement `RefNames`:

- `wuphf.connector` → the connector id itself, plus its platform
  (`"telegram-main (telegram)"`), from the snapshot.
- `wuphf.rule` → the rule id (038 keys rules by id; nothing nicer exists).
- `wuphf.chat` → `ObservedChat.title` for the matching `connector_id` and
  `chat_id`, via `list_observed_chats()`. Cache nothing in this plan; the
  call is a single indexed query and panels render rarely.

**Verify**: unit test `chat_names_come_from_the_observation_log` logs one
observed message with a chat title on `InMemoryStorage` and asserts the name.

Commit 2.

### Step 4: Mount the service

Add `botconf-grpc` to the root `[workspace.dependencies]` (git dependency on
`https://github.com/propheci/botconf`, same tag as `botconf`) and to
`wuphf-cli`. In `serve()`, after the settings store is loaded (035):

```rust
let settings_service = botconf_grpc::SettingsService::new(
    Arc::clone(&settings),
    WuphfAuthorizer::new(Arc::clone(&settings), connector_tokens.clone(), configuration.observer_token.clone()),
)
.with_names(WuphfNames::new(Arc::clone(&storage), Arc::clone(&settings)));
// …
.add_service(botconf_grpc::server(settings_service))
```

No `bearer_auth` interceptor on this service: `Authorize` does it, because it
needs to know which token was presented.

When `observer_token` is unset, log once:
`tracing::warn!("settings service: no observer token set; only connectors acting for their owners can change settings")`.

**Verify**: `cargo build --workspace --all-targets` → exit 0. End-to-end test
`settings_service_end_to_end` in `crates/wuphf-cli` (or `wuphf-runtime`'s
`tests.rs`, following its TCP harness at `tests.rs:486-500`): start the
service on `127.0.0.1:0` with one connector token and one owner id; with the
connector token and actor = the owner, `Set` a runtime key and see it in
`store.current()`; with actor = someone else, get `PERMISSION_DENIED` and no
change.

Commit 3.

### Step 5: Refs in the runtime settings

In 035's runtime settings list, change the kinds:

- Every rule field that names a connector → `Kind::Ref { domain:
  "wuphf.connector", many: false }`.
- Any setting that lists rule ids (none today; skip if absent) →
  `Kind::Ref { domain: "wuphf.rule", many: true }`.
- `notify` and chat fields inside rules stay `Kind::Json` (open question,
  option b).

Add a CLI test `ref_domains_are_wuphf_ones` that walks the catalog and asserts
every `Kind::Ref` domain starts with `wuphf.`.

### Step 6: Document

- `config/example.toml`: under the first `[[connectors]]`, add a commented
  `# owner_ids = ["704338780"]` with one line explaining it (platform user ids
  of the owner on this connector; who may change settings from this
  platform; never editable at runtime).
- `docs/architecture.md`: a "Settings" paragraph — the store in the core,
  the service, decisions 1–3 and F1.
- `docs/connector-protocol.md`: a "Settings" section — which token to send,
  the `actor` rule, the status codes (`UNAUTHENTICATED`, `PERMISSION_DENIED`),
  and that connectors must never mirror a settings command.
- `README.md`: one line under configuration pointing at the docs.

**Verify**: `grep -c owner_ids config/example.toml docs/connector-protocol.md` → each ≥ 1.

Commit 4.

### Step 7: Full gate

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

In `wuphf-cli`: `connector_owner_ids_parse`, `owner_ids_must_not_be_blank`,
`owner_ids_are_not_runtime_keys`, `ref_domains_are_wuphf_ones`,
`settings_service_end_to_end`.

## Done criteria

- [ ] `cargo test --workspace` exits 0; the cases above exist by name
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings;
      `cargo fmt --check` exits 0
- [ ] `owner_ids` cannot be changed through the settings store
- [ ] `git diff --stat 643829b..HEAD` for this plan's commits lists only
      in-scope paths
- [ ] `plans/README.md` status row updated, F1 and the open-question answer
      recorded in the dependency notes

## STOP conditions

- botconf-grpc's authorization hook cannot see which bearer token was
  presented, or gives no way to pass an actor id — the design needs both.
- botconf's tagged API differs materially from the shapes in "Current state"
  (names may differ; the concepts must exist).
- Plan 034 did not land per-connector tokens, or tokens are not resolvable to
  a connector id from request metadata.
- The owner picked option (a) for the open question.
- Mounting the service needs a change to `ConnectorTransport` or to routing.

## Maintenance notes

- `owner_ids` is per connector, not per platform: two Telegram connectors
  (two accounts) may have different owners.
- For a Telegram **user** session, the owner types from the session's own
  account, so that account's user id must be in `owner_ids` (041 sends it as
  the actor).
- Constant-time token comparison, TLS and per-key permissions (e.g. a
  co-owner who may only edit rendering) are deferred.
