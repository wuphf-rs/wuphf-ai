# Plan 034: Authenticate connectors and stop session takeover

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-runtime/src/connector.rs crates/wuphf-runtime/src/grpc.rs crates/wuphf-runtime/src/media.rs crates/wuphf-runtime/src/observer_service.rs crates/wuphf-cli/src/main.rs crates/wuphf-connector/src/client.rs connectors/shared/coreclient connectors/whatsapp-rust/src crates/wuphf-functions/src/config.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. Plan 035 moves `Configuration` out
> of `crates/wuphf-cli/src/main.rs` into a new `wuphf-settings` crate; if 035
> has landed, apply this plan's `ConnectorConfiguration` change there instead
> (same struct, new file) — that move alone is not a STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED — changes who may open connector and media sessions; a
  mistake locks every connector out (loud) or leaves the hole open (silent).
  Does not touch routing, dispatch planning or storage.
- **Depends on**: none. Must land before plan 039 (settings over connectors),
  which trusts the connector session to say who the acting user is.
- **Category**: security
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

Today any process that can reach the core's port can register as any
configured connector. The only check is that the first `Registration` frame
repeats the connector's id, platform and capabilities, none of which are
secret. The hub then *replaces* the live session with the newcomer, so the
impostor receives that connector's dispatches (every mirrored message) and can
inject events as if it were the platform. `MediaTransport` has no check at
all: anyone can upload blobs or download any media id.

The runtime-settings work (plans 035–043) will let a connector forward a
platform user's settings commands to the core. That is only safe if the core
knows the frames really come from the connector they claim to. This plan
closes the hole with a per-connector bearer token, and makes a second session
for a connector id that is still live fail instead of silently taking over.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | Every `[[connectors]]` entry gets a **required** `token_env`: the name of an environment variable holding that connector's token, mirroring `[[functions.providers]].token_env`. The core refuses to start if a connector's variable is unset or empty. |
| 2 | `ConnectorTransport.OpenSession` requires `authorization: Bearer <token>` and the token must be the one configured for the connector id in the registration frame. Wrong or missing token → `UNAUTHENTICATED`, and the hub never sees the attempt. |
| 3 | `MediaTransport` (upload and download) requires a bearer token that is **any** configured connector token **or any provider token** from `[[functions.providers]]` (the `mediaconv` provider uploads and downloads media). No token or an unknown one → `UNAUTHENTICATED`. |
| 4 | A registration for a connector id whose current session is **still live** is rejected with `ALREADY_EXISTS`; the live session is untouched. A session counts as live until its stream has ended. |
| 5 | Each connector config gains `core_token_env` (Go connectors, and `whatsapp-rust`). The provider reuses its existing `token_env` token for media. Tokens are never read from flags or files, only from the environment. |
| 6 | Token comparison stays `==`, like `bearer_auth` and the provider check (constant-time comparison is a deferred hardening item for all three together). |

### Decision 4 in detail: how a restarted connector gets back in (confirmed by the owner 2026-10-25)

The owner settled "reject a second live session instead of taking it over".
The question this plan had to answer is how a connector that legitimately
restarts recovers, given the core may not yet know its old session is gone:

- **Process restart or crash (the common case).** The old process exits, the
  kernel closes its socket, tonic's stream for the old session ends at once,
  and the session stops counting as live (Step 1 wires that up). By the time
  the new process has dialled and registered, the old session is gone.
- **Silent network loss (cable pulled, laptop sleep).** The core's HTTP/2
  keepalive (`functions.keepalive_interval_ms` / `keepalive_timeout_ms`,
  server-wide since plan 032, defaults 10 s / 20 s) ends the dead connection
  within about 30 s. Until then the new session gets `ALREADY_EXISTS`; both
  connector SDKs already reconnect with exponential backoff (1 s doubling to
  30 s), so the connector gets in on a later attempt without help.

The alternative, *token-authenticated takeover* (a newcomer with the right
token replaces the live session, as providers do), would also be safe once
tokens exist, and recovers instantly from silent network loss. This plan
chose rejection because it is what the owner settled, it is the stricter
behaviour (two processes running with the same id and token — a deployment
mistake — fail loudly instead of stealing each other's dispatches back and
forth), and the cost is a bounded ~30 s delay in a rare case. The owner
confirmed rejection on 2026-10-25, with the alternative noted; switching
later changes only Step 2's `register` check.

## Current state

- `crates/wuphf-runtime/src/connector.rs` — the `ConnectorHub` (one task
  owning all session state). Registration only compares the frame with
  config, then inserts unconditionally, replacing any live session:

  ```rust
  // connector.rs:91-110
  async fn register(&mut self, request: ConnectRequest) {
      let connector_id = request.registration.id.clone();
      if self.allowed.get(&connector_id) != Some(&request.registration) {
          warn!(/* … */ "rejecting connector session: registration does not match configuration");
          let _ = request.result_tx.send(Err(
              "connector registration does not match configuration".to_owned(),
          ));
          return;
      }
      info!(connector = %connector_id.0, "connector session registered");
      self.sessions.insert(
          connector_id.clone(),
          Session { output: request.output_tx },
      );
  ```

  `struct Session { output: mpsc::Sender<ConnectorOutput> }` (connector.rs:52-54).
  Each session gets a forwarding task (connector.rs:111-123) that pushes
  `(connector_id, input)` into the hub's shared `inbound_tx`. Inputs are
  keyed only by connector id, so after a takeover the *old* session's
  forwarder keeps delivering under the same id: `accept_input`
  (connector.rs:146-150) only checks `self.sessions.contains_key(&connector_id)`.
  The hub never learns that a session ended; a dead session stays in
  `sessions` and dispatches to it fail over to `held` only when
  `session.output.send` errors (connector.rs:212-222).

- `crates/wuphf-runtime/src/grpc.rs:28-85` — `GrpcConnectorService::open_session`.
  Reads the registration, calls `hub.connect`, spawns a reader task (frames →
  `connection.input`) and an outgoing task (`connection.output` → response
  stream). The `ConnectorConnection` (its `input` sender included) is moved
  whole into the outgoing task, which blocks on `connection.output.recv()`;
  so nothing closes when the client disconnects until the next dispatch send
  fails. There is no token check; the service is constructed as
  `GrpcConnectorService::new(handle)`.

- `crates/wuphf-runtime/src/media.rs:17-33` — `GrpcMediaService::new(storage,
  max_media_bytes, orphan_media_retention)`; no authentication anywhere.

- `crates/wuphf-runtime/src/observer_service.rs:265-285` — the existing
  interceptor:

  ```rust
  #[allow(clippy::result_large_err)]
  pub fn bearer_auth(token: Option<String>) -> impl tonic::service::Interceptor + Clone {
      move |request: Request<()>| -> Result<Request<()>, Status> {
          let Some(expected) = token.as_deref() else { return Ok(request); };
          let presented = request.metadata().get("authorization")
              .and_then(|value| value.to_str().ok())
              .and_then(|value| value.strip_prefix("Bearer "));
          match presented {
              Some(value) if value == expected => Ok(request),
              _ => Err(Status::unauthenticated("valid bearer token required")),
          }
      }
  }
  ```

- `crates/wuphf-functions/src/config.rs` — the pattern to mirror:
  `ProviderConfig.token_env: String` ("Env var holding this provider's
  token. Required.", line 55-56); `FunctionsConfig::resolve(self, env)` reads
  every token through an injectable `env` closure and fails with
  `"{owner} reads its token from {variable}, which is unset or empty"`
  (line 94); `Functions::authenticate(provider_id, token)` (line 336) is
  `pub(crate)`. Resolved provider tokens are private (`struct Provider { id,
  token, patterns }`, line 154-158).

- `crates/wuphf-cli/src/main.rs` — `ConnectorConfiguration { id, platform,
  supports_threads, supports_topic_create }` (lines 91-99); `serve()` builds
  `GrpcConnectorService::new(handle)` and `MediaTransportServer::new(media_service)`
  with no interceptor (lines 181-185); the keepalive comes from
  `functions.keepalive_interval()` (line 178-180).

- Go: `connectors/shared/coreclient/client.go:57-67` (`Options`) and
  `runSession` (line 182-200) dial with `grpc.NewClient(c.opts.CoreAddress,
  grpc.WithTransportCredentials(insecure.NewCredentials()))` and send the
  `Registration` frame; `connectors/shared/coreclient/media.go:20-26`
  `DialMedia(address string)`. `DialMedia` call sites:
  `connectors/telegram/internal/telegram/connector.go:34`,
  `connectors/whatsapp/internal/whatsapp/connector.go:50`,
  `connectors/instagram/internal/instagram/connector.go:40`,
  `providers/mediaconv/cmd/wuphf-mediaconv/main.go:33` (which already reads
  its provider token: `token := os.Getenv(cfg.TokenEnv)`, line 29-32). The
  shared `functions` client shows how metadata is attached:
  `metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+c.token)`
  (`connectors/shared/functions/client.go:60`).
- Go connector configs: `connectors/{telegram,whatsapp,instagram}/internal/config/config.go`
  (TOML structs; e.g. telegram's has `FunctionsTokenEnv string
  \`toml:"functions_token_env"\``) with `config.example.toml` next to each.
- Rust connector SDK: `crates/wuphf-connector/src/client.rs:36-48` (`Options`),
  `run_session` (line 123-150: `Endpoint::new(core_uri(...))`,
  `ConnectorTransportClient::new(channel)`, `transport.open_session(ReceiverStream::new(outbound_rx))`).
  `connectors/whatsapp-rust/src/config.rs` (`Config { core_address,
  connector_id, session_database, log_level, phone_number }`) and
  `src/main.rs:93` build `wuphf_connector::Options`. `whatsapp-rust` is its
  own cargo workspace and does not use `MediaTransport`.
- Tests: the in-process hub tests in `crates/wuphf-runtime/src/tests.rs`
  connect through `handle.connect(registration)` and include a reconnect
  after `drop(telegram)` (around line 470-485); the gRPC harness binds
  `TcpListener` on `127.0.0.1:0` and serves with `serve_with_incoming`
  (around lines 526-535 and 622-640). Go: `connectors/shared/coreclient/client_test.go`.
- `docs/connector-protocol.md:9-16` describes registration; line 98-99
  describes provider takeover.

Conventions: Rust 2024; modules start with `//!`; `thiserror`; `tracing` with
structured fields (`connector = %id.0`); Go uses zerolog and `go vet`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Build | `cargo build --workspace --all-targets` | exit 0 |
| Runtime tests | `cargo test -p wuphf-runtime` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline (147 at `643829b`) + new tests |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| whatsapp-rust | `cd connectors/whatsapp-rust && RUSTC_BOOTSTRAP=1 cargo build && RUSTC_BOOTSTRAP=1 cargo test` | exit 0 |
| Go (each of `connectors/shared`, `connectors/telegram`, `connectors/whatsapp`, `connectors/instagram`, `providers/mediaconv`) | `go build ./... && go vet ./... && go test ./...` | exit 0 |

`protoc` must be on PATH (`nix develop`). No proto changes are needed: the
token travels as gRPC metadata.

## Scope

**In scope**:
- `crates/wuphf-runtime/src/{connector.rs,grpc.rs,media.rs,lib.rs,tests.rs}`,
  a new `crates/wuphf-runtime/src/auth.rs`
- `crates/wuphf-functions/src/config.rs` (one public accessor)
- `crates/wuphf-cli/src/main.rs` (or `crates/wuphf-settings` if 035 landed)
- `crates/wuphf-connector/src/client.rs`, `crates/wuphf-connector/tests/session.rs`
- `connectors/whatsapp-rust/src/{config.rs,main.rs}`, its `config.example.toml`
- `connectors/shared/coreclient/{client.go,media.go,client_test.go}`
- `connectors/{telegram,whatsapp,instagram}/internal/config/config.go` (+
  tests), their `connector.go`, their `config.example.toml`, READMEs
- `providers/mediaconv/cmd/wuphf-mediaconv/main.go`
- `config/example.toml`, `scripts/dev-aliases.sh`, `scripts/dev-bin/*` (comments only)
- `docs/connector-protocol.md`, `docs/architecture.md` (documentation only)

**Out of scope** (do NOT touch):
- `crates/wuphf-core`, `crates/wuphf-storage`, `crates/wuphf-model` — identity
  and routing do not change.
- Proto files — no wire change.
- `FunctionTransport` / `ProviderTransport` auth — unchanged.
- The observer service and its `observer_token`.
- TLS, token rotation, constant-time comparison.

## Git workflow

- Commit on the current branch. Do not create a branch, push, or open a PR
  unless the operator instructed it.
- Commits (conventional style, as in `git log`):
  1. `feat(runtime): reject a second live connector session`
  2. `feat(runtime): require per-connector tokens on connector and media sessions`
  3. `feat(connectors): send the core token`
  4. `docs: describe connector authentication`

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ `147` at `643829b` (record whatever you get). Run the Go command in each Go
module and confirm green.

### Step 1: Make session liveness observable in the hub

Goal: the hub can tell, synchronously, whether a connector's current session
is still live, and ignores input from any session other than the current one.

In `connector.rs`:

1. Give every session a generation number. Add `next_session: u64` to
   `ConnectorHub` and `generation: u64` to `Session`.
2. Tag inbound input with the generation: change `inbound_tx`/`inbound_rx` to
   carry `(ConnectorId, u64, ConnectorInput)`; the per-session forwarding task
   sends its own generation. In `accept_input`, drop (with the existing
   `debug!`) input whose generation is not the current session's — this
   replaces the `contains_key` check and closes the stale-forwarder gap.
3. Liveness is `!session.output.is_closed()`: the receiving half of `output`
   is the `ConnectorConnection.output` the transport holds; it is dropped
   exactly when the transport is done with the session. Add
   `fn live_session(&self, id) -> Option<&Session>` returning the session only
   when its output is not closed.

In `grpc.rs` `open_session`, make the receiving half drop as soon as the
client's stream ends:

- Destructure `let ConnectorConnection { input, mut output } = connection;`
  so the original `input` sender is moved into the reader task (no clone kept
  elsewhere).
- Create a `tokio::sync::oneshot` (or `tokio_util::sync::CancellationToken`
  if already a dependency — check `crates/wuphf-runtime/Cargo.toml`; do not
  add `tokio-util` just for this) that the reader task fires when it returns
  for any reason (stream end, error, malformed frame, full channel).
- The outgoing task `select!`s between `output.recv()` and that signal and
  returns on the signal, dropping `output`.

**Verify**: `cargo build -p wuphf-runtime` → exit 0;
`cargo test -p wuphf-runtime` → all existing tests pass (behaviour unchanged
so far: `register` still inserts unconditionally).

### Step 2: Reject a registration while the current session is live

In `register`, after the configuration match and before inserting:

```rust
if self.live_session(&connector_id).is_some() {
    warn!(connector = %connector_id.0, "rejecting connector session: a session for this connector is still live");
    let _ = request.result_tx.send(Err(RegistrationError::AlreadyLive));
    return;
}
```

Make the error typed so the gRPC layer can map it: change the hub's result
type from `Result<(), String>` to `Result<(), RegistrationError>` with
`thiserror` variants `Mismatch` ("connector registration does not match
configuration"), `AlreadyLive` ("a session for this connector is still
live"), and `HubStopped`. `ConnectorHubHandle::connect` returns
`Result<ConnectorConnection, RegistrationError>`. In `grpc.rs` map `Mismatch`
→ `Status::failed_precondition` (as today), `AlreadyLive` →
`Status::already_exists`, `HubStopped` → `Status::unavailable`.

Keep everything after the insert as is (redelivery of the previous session's
unacknowledged dispatches and the `held` queue).

Add tests in `crates/wuphf-runtime/src/tests.rs`:

- `second_live_session_is_rejected` — connect telegram, connect again with the
  same registration → `Err(AlreadyLive)`; the first connection still receives
  the next dispatch.
- `reconnect_after_drop_is_accepted` — connect, `drop` the connection, connect
  again → `Ok`, and an unacknowledged dispatch is redelivered (the existing
  reconnect test around line 480 already does this; confirm it still passes
  unchanged and add an explicit assertion if it does not already check `Ok`).
- `input_from_a_replaced_session_is_ignored` — this cannot happen through the
  public API any more once takeover is gone; instead assert at the unit level:
  connect, drop, reconnect, then make the *old* forwarder's input arrive (send
  on a clone of the old `input` taken before the drop) and assert nothing
  reaches `runtime.inbound`. If the old sender is unreachable after drop by
  construction, delete this test and note it in the commit message.
- `grpc_second_session_is_already_exists` and
  `grpc_session_after_client_disconnect_is_accepted` — over the TCP harness:
  open a session, open a second → `ALREADY_EXISTS`; drop the first client's
  stream and channel, then open again → registered. Order events by awaiting
  the first stream's end on the server side (e.g. retry `open_session` in a
  loop bounded by a short `tokio::time::timeout` with `yield_now`, not
  `sleep`); the window between client drop and server noticing is a socket
  close, not a timer.

**Verify**: `cargo test -p wuphf-runtime` → all pass, new tests included.
Remove the `live_session` check temporarily and confirm
`second_live_session_is_rejected` fails, then restore it.

Commit 1.

### Step 3: Connector tokens in the configuration

1. `ConnectorConfiguration` gains `token_env: String` (required, no default).
   Doc comment: "Environment variable holding this connector's token. The
   connector presents it as `authorization: Bearer <token>`; required."
2. In `serve()`, resolve every connector's token with the same rule and
   message shape as providers (`connector {id} reads its token from
   {variable}, which is unset or empty`), failing startup. Build a
   `ConnectorTokens` (new type, `crates/wuphf-runtime/src/auth.rs`) mapping
   `ConnectorId → String`.
3. Add `pub fn provider_tokens(&self) -> impl Iterator<Item = &str>` to
   `wuphf_functions::Functions` (doc: "Every provider's token, for services
   providers also use, such as media.").
4. New module `crates/wuphf-runtime/src/auth.rs` (`//!` doc: who
   authenticates where), containing:
   - `pub fn bearer_token(metadata: &MetadataMap) -> Option<&str>` (the same
     parsing as `bearer_auth`; refactor `bearer_auth` to call it, behaviour
     unchanged).
   - `ConnectorTokens` with `new(HashMap<ConnectorId, String>)` and
     `fn authenticates(&self, id: &ConnectorId, token: Option<&str>) -> bool`.
   - `pub fn any_token_auth(tokens: Vec<String>) -> impl Interceptor + Clone`
     — accepts a request whose bearer token equals any of `tokens`; an empty
     list rejects everything (never "allow all": there is always at least one
     connector in a working config, and an empty list means misconfiguration).
5. `GrpcConnectorService::new(hub, tokens: Arc<ConnectorTokens>)`. In
   `open_session`, read the token from `request.metadata()` **before**
   `into_inner()`, parse the registration as today, then
   `if !tokens.authenticates(&registration.id, token) { return Err(Status::unauthenticated("unknown connector or wrong token")); }`
   before calling the hub.
6. In `serve()`, wrap media:
   `MediaTransportServer::with_interceptor(media_service, any_token_auth(connector_tokens ++ functions.provider_tokens()))`.
7. `config/example.toml`: add `token_env = "WUPHF_CONNECTOR_<ID>_TOKEN"` to
   every `[[connectors]]` entry (`WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN`, …) and
   rewrite the comment above `[[connectors]]`: the core only trusts a session
   that presents the connector's token *and* whose registration matches; a
   second session while one is live is refused.
8. Update the CLI tests that parse inline configs with `[[connectors]]`
   entries (there are none today besides `example_configuration_parses`; the
   example must still parse — now with `token_env`). Add
   `connector_without_token_env_is_rejected` (parse fails with a message
   naming `token_env`).

Tests in `wuphf-runtime`:

- `grpc_session_without_token_is_unauthenticated`,
  `grpc_session_with_another_connectors_token_is_unauthenticated`,
  `grpc_session_with_its_token_registers`.
- `media_requires_a_known_token` — upload without a token → `UNAUTHENTICATED`;
  with a connector token → OK; with a provider token → OK; download likewise.
- `any_token_auth_with_no_tokens_rejects` (unit test on the interceptor).

**Verify**: `cargo test --workspace` → all pass; `cargo clippy --workspace
--all-targets` → no warnings.

Commit 2.

### Step 4: Clients send the token

Rust SDK (`crates/wuphf-connector/src/client.rs`):

- `Options` gains `pub token: String` (doc: "The connector's core token, sent
  as `authorization: Bearer <token>` on the session").
- In `run_session`, wrap the stream in a `tonic::Request` and insert the
  header before `transport.open_session(request)`. Update
  `crates/wuphf-connector/tests/session.rs` for the new field and, if the test
  serves a real `GrpcConnectorService`, configure the matching token.

`whatsapp-rust`:

- `Config` gains `core_token_env: String` (required; doc as in the Go
  connectors). In `main.rs`, read it with `std::env::var`, fail with
  `"environment variable {name} (core_token_env) is unset or empty"`, and pass
  it as `Options.token`. Update `connectors/whatsapp-rust/config.example.toml`.

Go (`connectors/shared/coreclient`):

- Add a small `bearerCredentials` type implementing
  `credentials.PerRPCCredentials` (`GetRequestMetadata` returns
  `{"authorization": "Bearer " + token}`, `RequireTransportSecurity` returns
  `false`, since every core connection is `insecure` today) and a helper
  `func WithToken(token string) grpc.DialOption`.
- `Options` gains `Token string`; `runSession` dials with `WithToken`.
- `DialMedia(address, token string)`.
- `client_test.go`: assert the server side sees the header (use a test server
  with a stream interceptor capturing metadata).

Each Go connector (`telegram`, `whatsapp`, `instagram`):

- `config.go`: `CoreTokenEnv string \`toml:"core_token_env"\``; `Validate`
  requires it non-empty. Add the field to `config.example.toml` with a comment
  ("Environment variable holding this connector's core token; must match the
  core's `[[connectors]].token_env` value for this connector id").
- At startup (`connector.go`), read `os.Getenv(cfg.CoreTokenEnv)` and fail if
  empty (same wording as mediaconv's check); pass it to `coreclient.Options`
  and `coreclient.DialMedia`.
- Extend each connector's config test for the new required field.

`providers/mediaconv/cmd/wuphf-mediaconv/main.go`: pass its existing provider
`token` to `coreclient.DialMedia(cfg.CoreAddress, token)`.

**Verify**: the Go command in every Go module → exit 0; whatsapp-rust build
and test → exit 0; `cargo test --workspace` → all pass.

Commit 3.

### Step 5: Document

- `docs/connector-protocol.md`: in the session section, state that
  `OpenSession` requires the connector's bearer token, that the token must
  belong to the connector id in the registration, and that a second session
  while one is live gets `ALREADY_EXISTS` (the client keeps retrying with
  backoff; a dead connection is noticed within the keepalive window). In the
  media section, state which tokens `MediaTransport` accepts.
- `docs/architecture.md`: one sentence in the trust/sessions paragraph (find
  it by searching for "Registration" or "session").
- Each connector README that shows a run command: mention exporting the
  token variable. `scripts/dev-aliases.sh` help text: list the token
  variables next to the existing `WUPHF_PROVIDER_MEDIACONV_TOKEN` note.

**Verify**: `grep -c "ALREADY_EXISTS\|Bearer" docs/connector-protocol.md` → ≥ 2.

Commit 4.

### Step 6: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0; total = Step 0 + new tests. Every Go module green. Then a manual
smoke test: start the core with `config/example.toml` and the token
variables exported, start the Telegram connector with its token → registers;
start a second copy → logs `ALREADY_EXISTS` and keeps retrying; kill the
first → the second registers within one backoff period.

## Test plan

In `crates/wuphf-runtime/src/tests.rs`:

1. `second_live_session_is_rejected`
2. `reconnect_after_drop_is_accepted` (or the strengthened existing test)
3. `input_from_a_replaced_session_is_ignored` (or removed, see Step 2)
4. `grpc_second_session_is_already_exists`
5. `grpc_session_after_client_disconnect_is_accepted`
6. `grpc_session_without_token_is_unauthenticated`
7. `grpc_session_with_another_connectors_token_is_unauthenticated`
8. `grpc_session_with_its_token_registers`
9. `media_requires_a_known_token`
10. `any_token_auth_with_no_tokens_rejects`

Plus `connector_without_token_env_is_rejected` in the CLI (or
`wuphf-settings`) tests, the updated `crates/wuphf-connector/tests/session.rs`,
`coreclient` header test, and each Go connector's config test.

## Done criteria

- [ ] `cargo test --workspace` exits 0; total = baseline + new tests; the
      cases above exist by name
- [ ] `cargo clippy --workspace --all-targets` and `cargo fmt --check` exit 0
- [ ] Every Go module builds, vets and tests green; whatsapp-rust builds and
      tests green
- [ ] Removing the live-session check makes `second_live_session_is_rejected`
      fail; removing the token check makes
      `grpc_session_with_another_connectors_token_is_unauthenticated` fail
- [ ] `git diff --stat 643829b..HEAD` lists only in-scope paths
- [ ] `plans/README.md` status row updated

## STOP conditions

- The existing reconnect test (drop then connect) fails after Step 2 and the
  cause is not the output-closed signal — report the ordering problem rather
  than adding sleeps.
- tonic 0.12 does not drop the reader stream promptly when a client
  disconnects (the `grpc_session_after_client_disconnect_is_accepted` test can
  only pass with a timer) — report.
- `MediaTransport` turns out to be used by a caller that has neither a
  connector nor a provider token (search `NewMediaTransportClient`,
  `MediaTransportClient` in all Go and Rust code first) — report who.
- Any step seems to need a proto change or a change in `wuphf-core`.

## Maintenance notes

- Plan 039 builds on this: settings commands arrive over an authenticated
  connector session, and the core trusts the session's connector id, never a
  field in the frame.
- Deferred hardening, together for all token checks (observer, invoke,
  provider, connector, media): constant-time comparison, token rotation (two
  valid tokens during a switch), TLS.
- If the owner later prefers authenticated takeover over rejection, change
  only the `live_session` check in `register` to "replace and close the old
  session" and flip tests 1 and 4.
- The media interceptor accepts any connector's or provider's token, so a
  connector can still download media another connector uploaded. Per-owner
  media ACLs are out of scope; media ids are unguessable UUIDs.
