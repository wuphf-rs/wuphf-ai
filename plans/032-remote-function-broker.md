# Plan 032: Add a remote function broker to the core

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 2c92f5a..HEAD -- crates/wuphf-proto crates/wuphf-cli Cargo.toml config/example.toml proto/go crates/wuphf-runtime/src/observer_service.rs`
> Renames from plans 026–031 in `crates/wuphf-cli/src/main.rs` are expected
> and harmless (this plan only adds to that file). What must still hold: the
> proto files all use `package wuphf.connector.v1`; `crates/wuphf-proto/build.rs`
> lists the proto files in one `compile_protos(&[…])` call;
> `crates/wuphf-cli/src/main.rs` builds one `tonic::transport::Server` in
> `serve()`; `wuphf_runtime::observer_service::bearer_auth` exists with the
> signature shown below. Otherwise STOP.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED — new network surface with its own auth; does not touch
  routing, dispatch, storage or the connector protocol
- **Depends on**: none (independent of 026–031; rebases trivially on them)
- **Category**: direction
- **Planned at**: commit `2c92f5a`, 2026-10-22

## Why this matters

Sticker conversion (ffmpeg, ImageMagick, rlottie) is compiled into the Go
connectors today, so every connector in another language (the Rust WhatsApp
connector already exists) must re-implement it, and helper code can only be
written in Go. This plan adds a broker to the core: a **provider** process in
any language dials the core, registers named functions, and serves calls; any
**caller** (a connector, the CLI, another provider) invokes them through the
core by name. Plan 033 then moves sticker conversion into the first provider.

The core only brokers. It never calls a function itself, and the broker lives
in its own crate that does not depend on `wuphf-core` or `wuphf-storage`, so
the correctness path (identity, routing, dispatch, acks) cannot come to depend
on whether a provider is running.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | A **general registry**: any provider registers any function name it is authorized for. |
| 2 | The core is a **broker**: every call goes caller → core → provider → core → caller. Providers dial in; they never listen. |
| 3 | Calls carry a small **JSON body inline** plus a list of **media ids**. Bulky data travels through the existing `MediaTransport` (`UploadMedia`/`DownloadMedia`), never in the body. |
| 4 | Providers are a **new peer kind** with their own gRPC service, separate from `ConnectorTransport`. (Merging both into one role-based session is a later plan.) |
| 5 | Functions are named `dotted.name` plus an integer **major** version; `(name, major)` is the key. Bodies are JSON. A provider sends a JSON Schema per function at registration; the core stores and lists it but does **not** validate against it in v1. |
| 6 | With **no provider** connected, a call **waits** for one until its deadline. A second provider claiming a function another provider owns is **rejected**. |
| 7 | Deadlines, cancellation and failure: the call deadline is the caller's gRPC deadline capped by a core maximum (default used when the caller sets none); waiting counts against it. Caller cancel → `CancelCall` to the provider. Provider lost mid-call → re-queue if the function is declared `idempotent`, else fail. HTTP/2 keepalive detects dead peers. A provider reconnecting with the **same provider id takes over** its old session. **Every timeout is configurable.** |
| 8 | Providers authenticate with a **per-provider token** and may only register names matching their configured patterns. Callers are unrestricted, optionally behind one shared invoke token. |
| 9 | Only callers outside the core invoke functions (v1). |
| 10 | No persistence: calls are traced with `tracing`; `ListFunctions` RPC and a `wuphf functions list` CLI command show state. Limits (body cap, per-provider concurrency, queue length) are configurable. |

Two consequences the executor must implement exactly:

- **Unconfigured name → fail fast.** "Wait for a provider" only applies when
  some configured provider is *allowed* to register the name. If no
  `[[functions.providers]]` pattern matches it, `Invoke` returns `NOT_FOUND`
  immediately. ("Not deployed" is fast; "deployed but down" waits.)
- **Takeover beats rejection.** A registration is rejected as a duplicate only
  when a *different* provider id owns the function. The same provider id
  reconnecting replaces its old session (whose in-flight calls are then
  treated as lost — re-queued or failed per rule 7).

## Current state

- `crates/wuphf-proto/` — all `.proto` files share
  `package wuphf.connector.v1;` and
  `option go_package = "github.com/akshettrj/wuphf/proto/go/connector/v1;wuphfpb";`.
  `build.rs` compiles them in one call:

  ```rust
  // crates/wuphf-proto/build.rs
  tonic_build::configure()
      .build_server(true)
      .build_client(true)
      .boxed(".wuphf.connector.v1.ServerFrame.body.message_create")
      .compile_protos(
          &["common.proto", "media.proto", "client.proto", "server.proto",
            "connector.proto", "observer.proto"],
          &["."],
      )?;
  ```
  `crates/wuphf-proto/src/lib.rs` is `pub mod connector { tonic::include_proto!("wuphf.connector.v1"); }`.
  `observer.proto` is the model for a self-describing service with comments.

- `proto/go/connector/v1/*.pb.go` — committed Go bindings. They are produced
  by this command, verified at planning time to reproduce the committed files
  byte-for-byte (protoc 34.1, protoc-gen-go 1.36.11, protoc-gen-go-grpc 1.6.2
  from the nix dev shell). `observer.proto` is deliberately **not** generated
  for Go:

  ```sh
  cd crates/wuphf-proto && protoc \
    --go_out=../../proto/go --go_opt=module=github.com/akshettrj/wuphf/proto/go \
    --go-grpc_out=../../proto/go --go-grpc_opt=module=github.com/akshettrj/wuphf/proto/go \
    common.proto media.proto client.proto server.proto connector.proto
  ```

- `crates/wuphf-runtime/src/connector.rs` — `ConnectorHub`: the repo's
  pattern for a session registry. A single task owns all state behind `&mut
  self`; a cloneable `ConnectorHubHandle` sends it commands over `mpsc`;
  registration replies via `oneshot`. **Model the function hub on it.**
- `crates/wuphf-runtime/src/grpc.rs:28-85` — `GrpcConnectorService::open_session`:
  reads the first frame as the registration, registers with the hub, spawns a
  reader task for later frames, returns a `ReceiverStream` of outgoing frames.
  **Model `OpenProviderSession` on it.**
- `crates/wuphf-runtime/src/observer_service.rs:264-285` — the bearer-token
  interceptor to reuse for `FunctionTransport`:

  ```rust
  pub fn bearer_auth(token: Option<String>) -> impl tonic::service::Interceptor + Clone
  // None → allow all; Some(t) → require `authorization: Bearer <t>`,
  // else Status::unauthenticated("valid bearer token required")
  ```

- `crates/wuphf-cli/src/main.rs` — `Configuration` (serde, TOML) and
  `serve()`, which builds every service and one `Server::builder()`. The
  observer's token pattern: `observer_token: Option<String>` plus a
  `tracing::warn!` when unset. `Command` has one variant, `Serve { config }`.
  Its tests parse `config/example.toml` (`example_configuration_parses`).
- `crates/wuphf-runtime/src/tests.rs:486-500` — the in-process gRPC test
  harness: bind `TcpListener` on `127.0.0.1:0`, `Server::builder()
  .add_service(…).serve_with_incoming(TcpListenerStream::new(listener))`,
  connect a generated client to `http://{address}`. **Model the tests on it.**
- tonic is `0.12.3`. `Server` has `http2_keepalive_interval(Option<Duration>)`
  and `http2_keepalive_timeout(Option<Duration>)` (server-wide). tonic's
  server already honors the caller's `grpc-timeout` header by dropping the
  handler future when it expires (returning `Status::cancelled("Timeout
  expired")`); the plan relies on that drop for cancellation.
- Workspace manifest `Cargo.toml` lists members and `[workspace.dependencies]`
  (`serde`, `serde_json`, `thiserror`, `tokio`, `tokio-stream`, `tonic`,
  `tracing`, `uuid` with `v4` are all already there).

Conventions: Rust 2024 edition; every module starts with a `//!` doc comment;
errors via `thiserror`; logs via `tracing` with structured fields
(`connector = %id.0`); tests are `#[tokio::test]` in a `tests.rs` module.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Build | `cargo build --workspace --all-targets` | exit 0 |
| New crate tests | `cargo test -p wuphf-functions` | exit 0, all pass |
| CLI tests | `cargo test -p wuphf-cli` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; 111 + new tests (baseline 111 at `2c92f5a`) |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| Go bindings | the `protoc` command in Step 2 | exit 0 |
| Go build | `cd proto/go && go build ./... && go vet ./...` | exit 0 |

`protoc` must be on PATH (`nix develop`).

## Scope

**In scope**:
- `crates/wuphf-proto/functions.proto` (create), `crates/wuphf-proto/build.rs`
- `proto/go/connector/v1/functions.pb.go`, `functions_grpc.pb.go` (generated)
- `crates/wuphf-functions/**` (create: `Cargo.toml`, `src/lib.rs`,
  `src/config.rs`, `src/hub.rs`, `src/service.rs`, `src/tests.rs`)
- root `Cargo.toml`, `Cargo.lock`
- `crates/wuphf-cli/Cargo.toml`, `crates/wuphf-cli/src/main.rs`
- `config/example.toml`
- `README.md`, `docs/architecture.md`, `docs/connector-protocol.md`,
  `docs/index.html` (documentation only)

**Out of scope** (do NOT touch):
- `crates/wuphf-core`, `crates/wuphf-storage`, `crates/wuphf-router`,
  `crates/wuphf-model` — the broker must not depend on them (decision 9).
- `crates/wuphf-runtime` except *importing* `bearer_auth` from the CLI. Do not
  move the broker into the runtime crate.
- `ConnectorTransport` / `client.proto` / `server.proto` — no connector
  protocol changes.
- Go connectors and `connectors/mediaconv` — plan 033.
- Validating bodies against the declared JSON Schema, persisted call history,
  a metrics exporter, per-caller ACLs, TLS — all deferred (see Maintenance
  notes).

## Git workflow

- Branch: `advisor/032-remote-function-broker`.
- Commits (conventional style, as in `git log`: `feat: …`, `docs: …`):
  1. `feat(proto): add FunctionTransport and ProviderTransport`
  2. `feat: add wuphf-functions broker crate`
  3. `feat(cli): serve remote functions and add wuphf functions list`
  4. `docs: describe remote functions`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`
→ `111` at `2c92f5a` (more if other plans landed; record the number).

### Step 1: Write `functions.proto`

Create `crates/wuphf-proto/functions.proto` with exactly these definitions
(comments may be expanded, names and field numbers may not change):

```proto
syntax = "proto3";

package wuphf.connector.v1;

option go_package = "github.com/akshettrj/wuphf/proto/go/connector/v1;wuphfpb";

// Remote functions. A provider process registers named functions over a
// ProviderTransport session; any client calls them through FunctionTransport.
// The core only brokers calls; it never calls a function itself. Bodies are
// UTF-8 JSON capped by the core; bulky data travels as media ids through
// MediaTransport.
service FunctionTransport {
  // Call a function. When no provider is connected the call waits for one
  // until its deadline: the caller's gRPC deadline, capped by the core, or the
  // core's default when the caller sets none. NOT_FOUND when no configured
  // provider may serve the name.
  rpc Invoke(InvokeRequest) returns (InvokeResponse);
  rpc ListFunctions(ListFunctionsRequest) returns (ListFunctionsResponse);
}

service ProviderTransport {
  // The first client frame must be a ProviderHello. Authenticated with the
  // provider's own token as `authorization: Bearer <token>`.
  rpc OpenProviderSession(stream ProviderClientFrame) returns (stream ProviderServerFrame);
}

message FunctionRef {
  // Lowercase dotted name, e.g. "media.convert".
  string name = 1;
  // Major version, >= 1. A breaking change to the body format is a new major.
  uint32 major = 2;
}

message InvokeRequest {
  FunctionRef function = 1;
  // UTF-8 JSON.
  bytes args = 2;
  repeated string media_ids = 3;
  // Free-form caller label for logs (e.g. a connector id). Not authenticated.
  string caller = 4;
}

message InvokeResponse {
  // UTF-8 JSON.
  bytes result = 1;
  repeated string media_ids = 2;
  string provider_id = 3;
}

message FunctionDeclaration {
  FunctionRef function = 1;
  // JSON Schema documents describing args and result. Listed, not enforced.
  string request_schema = 2;
  string response_schema = 3;
  // Safe to run again if the provider is lost mid-call; such calls are
  // re-queued instead of failed.
  bool idempotent = 4;
  // Calls this provider runs at once for this function; 0 means 1, at most 1024.
  uint32 max_concurrency = 5;
  // Suggested default deadline. Core configuration overrides it and the core
  // maximum caps it.
  optional uint32 default_deadline_ms = 6;
}

message ProviderHello {
  string provider_id = 1;
  repeated FunctionDeclaration functions = 2;
}

message ProviderClientFrame {
  oneof body {
    ProviderHello hello = 1;
    CallResult result = 2;
  }
}

message ProviderServerFrame {
  oneof body {
    ProviderCall call = 1;
    CancelCall cancel = 2;
  }
}

message ProviderCall {
  string call_id = 1;
  FunctionRef function = 2;
  bytes args = 3;
  repeated string media_ids = 4;
  // The provider should give up after this instant (Unix epoch milliseconds).
  int64 deadline_unix_ms = 5;
  string caller = 6;
}

// The caller gave up or its deadline passed; stop working on the call. A
// result sent afterwards is ignored.
message CancelCall {
  string call_id = 1;
}

enum CallErrorCode {
  // Treated as FAILED.
  CALL_ERROR_CODE_UNSPECIFIED = 0;
  CALL_ERROR_CODE_INVALID_ARGUMENT = 1;
  CALL_ERROR_CODE_UNSUPPORTED = 2;
  CALL_ERROR_CODE_FAILED = 3;
}

message CallError {
  CallErrorCode code = 1;
  string message = 2;
}

message CallSuccess {
  // UTF-8 JSON.
  bytes result = 1;
  repeated string media_ids = 2;
}

message CallResult {
  string call_id = 1;
  oneof outcome {
    CallSuccess success = 2;
    CallError error = 3;
  }
}

message ListFunctionsRequest {}

message FunctionStatus {
  FunctionRef function = 1;
  // Empty when no provider is connected (calls are queued, waiting).
  string provider_id = 2;
  // Unset when no provider is connected.
  FunctionDeclaration declaration = 3;
  uint32 in_flight = 4;
  uint32 queued = 5;
  // Effective limit after core configuration; 0 when no provider.
  uint32 max_concurrency = 6;
  uint64 completed = 7;
  uint64 failed = 8;
  uint64 abandoned = 9;
}

message ListFunctionsResponse {
  repeated FunctionStatus functions = 1;
}
```

Add `"functions.proto"` to the `compile_protos` list in
`crates/wuphf-proto/build.rs` (after `"observer.proto"`).

**Verify**: `cargo build -p wuphf-proto` → exit 0;
`grep -h "pub mod .*_transport_server" target/debug/build/wuphf-proto-*/out/*.rs | sort -u`
→ includes `function_transport_server` and `provider_transport_server`
(next to the existing connector, media and observer ones).

### Step 2: Generate the Go bindings

Run the generation command with `functions.proto` added. It regenerates the
existing files too; they must not change:

```sh
cd crates/wuphf-proto && protoc \
  --go_out=../../proto/go --go_opt=module=github.com/akshettrj/wuphf/proto/go \
  --go-grpc_out=../../proto/go --go-grpc_opt=module=github.com/akshettrj/wuphf/proto/go \
  common.proto media.proto client.proto server.proto connector.proto functions.proto
```

**Verify**: `git status --short proto/go` → exactly two new files,
`proto/go/connector/v1/functions.pb.go` and `functions_grpc.pb.go`, and no
modified files; `cd proto/go && go build ./... && go vet ./...` → exit 0.

Commit 1.

### Step 3: Create the `wuphf-functions` crate and its config

Add `"crates/wuphf-functions"` to workspace `members` (after
`"crates/wuphf-proto"`) and `wuphf-functions = { path = "crates/wuphf-functions" }`
to `[workspace.dependencies]` (alphabetical position).

`crates/wuphf-functions/Cargo.toml`:

```toml
[package]
name = "wuphf-functions"
description = "Remote function broker: provider sessions, call queueing and Invoke"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "sync", "time"] }
tokio-stream.workspace = true
tonic.workspace = true
tracing.workspace = true
uuid.workspace = true
wuphf-proto.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["net"] }
tokio-stream = { workspace = true, features = ["net"] }
```

`src/lib.rs`: a `//!` doc comment summarizing the design-decisions table in
three or four sentences, then `mod config; mod hub; mod service; #[cfg(test)] mod tests;`
and re-exports: `pub use config::{ConfigError, FunctionKey, FunctionOverride, Functions, FunctionsConfig, ProviderConfig};`
`pub use hub::{FunctionHub, FunctionHubHandle};`
`pub use service::{GrpcFunctionService, GrpcProviderService};`.

`src/config.rs` — the TOML shape (embedded in the core config as
`[functions]`) and its resolved form:

```rust
/// `[functions]` in the core configuration. All durations are milliseconds.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FunctionsConfig {
    pub default_deadline_ms: u64,        // default 30_000
    pub max_deadline_ms: u64,            // default 60_000
    pub keepalive_interval_ms: u64,      // default 10_000
    pub keepalive_timeout_ms: u64,       // default 20_000
    pub max_body_bytes: usize,           // default 1 MiB (1_048_576)
    pub max_queued_calls: usize,         // default 256, per (name, major)
    /// Env var holding the shared token callers present to FunctionTransport.
    pub invoke_token_env: Option<String>,
    pub providers: Vec<ProviderConfig>,
    /// Keyed by function name (applies to every major).
    pub overrides: BTreeMap<String, FunctionOverride>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    /// Env var holding this provider's token. Required.
    pub token_env: String,
    /// Names this provider may register: exact names, `prefix.*`, or `*`.
    pub functions: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FunctionOverride {
    pub default_deadline_ms: Option<u64>,
    pub max_deadline_ms: Option<u64>,
    pub max_concurrency: Option<u32>,
    pub max_body_bytes: Option<usize>,
    pub max_queued_calls: Option<usize>,
}
```

`impl Default for FunctionsConfig` returns the defaults above with empty
`providers`/`overrides` and `invoke_token_env: None`.

`FunctionsConfig::resolve(self, env: impl Fn(&str) -> Option<String>) -> Result<Functions, ConfigError>`
produces the resolved, validated form. It must reject (each a distinct
`ConfigError` variant with a message naming the offending item):
- any timing value of `0`; `default_deadline_ms > max_deadline_ms`; the same
  check per override (an override's default against the override's max if
  set, else the global max); `max_body_bytes == 0`; `max_queued_calls == 0`
- an empty or duplicate provider `id`
- a provider with an empty `functions` list, or a pattern that is not `*`, a
  valid name, or a valid name followed by `.*`
- an override key that is not a valid name
- a `token_env` (or `invoke_token_env`) whose variable is unset or empty

A **valid name** matches `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$` (at least two
dotted segments). Implement the check by hand (split on `.`, check each
segment); do not add a regex dependency.

`Functions` (the resolved form) holds `Duration`s and exposes:
- `pub fn invoke_token(&self) -> Option<&str>`
- `pub fn keepalive_interval(&self) -> Duration`, `keepalive_timeout()`
- `pub(crate) fn authenticate(&self, provider_id: &str, token: Option<&str>) -> bool`
  — true only for a configured provider whose token equals `token`.
- `pub(crate) fn may_register(&self, provider_id: &str, name: &str) -> bool`
- `pub(crate) fn is_servable(&self, name: &str) -> bool` — some configured
  provider may register `name` (drives the fail-fast `NOT_FOUND`).
- `pub(crate) fn deadline(&self, name: &str, requested: Option<Duration>, suggested: Option<Duration>) -> Duration`
  = `min(requested.unwrap_or(default), max)` where `max` = override max or
  global max, and `default` = override default, else `suggested`, else the
  global default.
- `pub(crate) fn max_concurrency(&self, name: &str, declared: u32) -> u32`
  = `min(max(declared, 1), override.max_concurrency.unwrap_or(u32::MAX))`.
- `pub(crate) fn max_body_bytes(&self, name: &str) -> usize`,
  `max_queued_calls(&self, name: &str) -> usize` (override, else global).

Pattern matching: `*` matches every name; `media.*` matches names starting
with `media.`; anything else matches only itself.

`FunctionKey { name: String, major: u32 }` with `Display` as `name@major`
(e.g. `media.convert@1`) and
`pub(crate) fn from_proto(r: Option<pb::FunctionRef>) -> Result<Self, String>`
that rejects a missing ref, an invalid name, or `major == 0`.

For tests, give `Functions` a `#[cfg(test)] pub(crate) fn for_tests(config: FunctionsConfig, tokens: &[(&str, &str)]) -> Self`
that resolves with `env` looking up `tokens` (pairs of env-var name → value).

**Verify**: `cargo build -p wuphf-functions` → exit 0 (with `hub.rs`,
`service.rs` as empty `//!` stubs for now).

### Step 4: Implement the hub

`src/hub.rs` — one task owning all broker state, modeled on `ConnectorHub`.

Types:

```rust
pub struct FunctionHub { /* state below; constructed with Arc<Functions> */ }
#[derive(Clone)]
pub struct FunctionHubHandle {
    commands: mpsc::Sender<Command>,               // capacity 256
    abandons: mpsc::UnboundedSender<CallId>,       // never blocks; used from Drop
}
pub(crate) struct SessionTicket { pub provider_id: String, pub seq: u64 }
pub(crate) struct Call {
    pub id: CallId,                    // uuid v4 as string
    pub key: FunctionKey,
    pub args: Vec<u8>,
    pub media_ids: Vec<String>,
    pub caller: String,
    pub requested: Option<Duration>,   // from grpc-timeout
}
pub(crate) enum CallOutcome {
    Completed { result: Vec<u8>, media_ids: Vec<String>, provider_id: String, queued_for: Duration },
    Failed(CallFailure),
}
pub(crate) enum CallFailure {
    Provider { code: pb::CallErrorCode, message: String, provider_id: String },
    ProviderLost { provider_id: String },
}
pub(crate) enum InvokeRejection { NotServable, QueueFull { limit: usize } }
pub(crate) enum RegisterRejection {
    Invalid(String),                    // bad name/major/duplicate key/no functions/concurrency > 1024
    NotPermitted { function: String },  // outside the provider's patterns
    Owned { function: String, owner: String },
}
pub(crate) enum AbandonReport {
    Queued { provider_connected: bool },
    InFlight { provider_id: String },
    Unknown,
}
```

`FunctionHub::new(functions: Arc<Functions>) -> (Self, FunctionHubHandle)` and
`pub async fn run(mut self)`, a `tokio::select!` loop over `commands` and
`abandons` that exits when the command channel closes.

State held by the hub:
- `sessions: HashMap<String /*provider id*/, Session { seq: u64, output: mpsc::Sender<pb::ProviderServerFrame>, keys: Vec<FunctionKey> }>`
- `owners: HashMap<FunctionKey, Owner { provider_id, seq, declaration: pb::FunctionDeclaration, in_flight: u32, limit: u32 }>`
- `queues: HashMap<FunctionKey, VecDeque<Queued>>` where
  `Queued { call: Call, deadline_unix_ms: i64, enqueued_at: Instant, reply: oneshot::Sender<CallOutcome> }`
- `running: HashMap<CallId, Running { queued: Queued, provider_id, seq }>`
- `stats: HashMap<FunctionKey, Stats { completed, failed, abandoned }>`
- `next_seq: u64`

Handle methods (each sends a command and awaits a `oneshot` reply, except
`abandon`):

- `register(&self, provider_id: String, functions: Vec<pb::FunctionDeclaration>, output) -> Result<SessionTicket, RegisterRejection>`
  1. Validate: non-empty; every `function` parses as a `FunctionKey`; no key
     twice; `max_concurrency <= 1024`; `functions.may_register(provider_id,
     name)` for every name. Reject the **whole** registration on any failure.
  2. If any key is owned by a **different** provider id → `Owned`.
  3. If `provider_id` already has a session → **takeover**: run "drop
     session" (below) for the old one, then continue. Log
     `info!(provider = %id, "provider session replaced by a new session")`.
  4. Allocate `seq = next_seq++`; insert the session and one `Owner` per key
     with `limit = functions.max_concurrency(name, declared)`.
  5. Drain every registered key (below). Log
     `info!(provider = %id, functions = %list, "provider session registered")`.
- `invoke(&self, call: Call) -> Result<(Duration, oneshot::Receiver<CallOutcome>), InvokeRejection>`
  1. If `!functions.is_servable(&key.name)` → `NotServable`.
  2. `deadline = functions.deadline(name, call.requested, owner's declared
     default_deadline_ms if an owner exists)`.
  3. Prune the key's queue of entries whose `reply.is_closed()`; if
     `len >= functions.max_queued_calls(name)` → `QueueFull`.
  4. Push back a `Queued` (`deadline_unix_ms` = now + deadline in Unix ms);
     drain the key; return `(deadline, receiver)`.
- `result(&self, ticket: SessionTicket, result: pb::CallResult)` (no reply):
  look up `running[call_id]`. If absent, or its `(provider_id, seq)` differs
  from the ticket, log `debug!` and ignore (a late result after cancel or
  takeover). Otherwise remove it, decrement the owner's `in_flight` (only if
  the owner still has that `seq`), and reply:
  - `success`: if `result.len() > max_body_bytes(name)` or the bytes are not
    valid JSON (`serde_json::from_slice::<serde::de::IgnoredAny>`), reply
    `Failed(Provider { code: Failed, message: "provider result exceeds the
    body cap" / "provider result is not valid JSON" })`; else `Completed`.
  - `error`: `Failed(Provider { code, message })` (map `Unspecified` →
    `Failed`). A missing `outcome` is `Failed` with message "empty result".
  Update `stats` (completed / failed); drain the key.
- `closed(&self, ticket: SessionTicket)` (no reply): if the session for
  `ticket.provider_id` still has `ticket.seq`, run "drop session".
- `abandon(&self, id: CallId)` — `self.abandons.send(id)`, ignoring errors.
  Synchronous; safe in `Drop`.
- `abandon_and_report(&self, id: CallId) -> AbandonReport` — same as abandon
  but replies with what state the call was in.
- `list(&self) -> Vec<pb::FunctionStatus>` — one entry per key that has an
  owner, a non-empty queue, or stats; sorted by `(name, major)`.

Abandon (both forms): if the call is in `running` → remove it, decrement the
owner's `in_flight`, `try_send` a `CancelCall` to the session (ignore
failure), `stats.abandoned += 1`, drain the key, report `InFlight`. If it is
in a queue → remove it, `stats.abandoned += 1`, report `Queued`. Else
`Unknown`. The slot is released immediately even if the provider keeps
working; its late result is ignored by `result`.

**Drain(key)**: while the key has an owner with `in_flight < limit` and a
non-empty queue: pop the front; skip it if `reply.is_closed()`; `try_send` a
`ProviderCall` to the owner's session. On send failure the session is broken:
push the popped call back to the **front**, run "drop session" for the owner,
and stop. On success move it into `running` and `in_flight += 1`.

**Drop session(provider_id)**: remove the session and every `Owner` it holds.
For each `running` call of that `(provider_id, seq)`: if its owner's
declaration was `idempotent` and `!reply.is_closed()`, push it to the
**front** of its key's queue (it keeps waiting within its original deadline);
otherwise reply `Failed(ProviderLost)` and `stats.failed += 1`. Log
`warn!(provider = %id, requeued, failed, "provider session ended")`. Queued
calls stay queued (decision 6: they wait for the next provider).

Output channel sizing: the service creates the provider's output channel with
capacity `2 * Σ max_concurrency(declared) + 16` (calls ≤ limit, cancels ≤
limit), so `try_send` only fails when the provider stops reading its stream —
which the plan treats as a broken session.

**Verify**: `cargo build -p wuphf-functions` → exit 0;
`cargo clippy -p wuphf-functions --all-targets` → no warnings.

### Step 5: Implement the gRPC services

`src/service.rs`:

```rust
pub struct GrpcFunctionService { hub: FunctionHubHandle, functions: Arc<Functions> }
pub struct GrpcProviderService { hub: FunctionHubHandle, functions: Arc<Functions> }
```
each with `pub fn new(hub, functions) -> Self`.

`FunctionTransport::invoke`:
1. `requested = grpc_timeout(request.metadata())` — parse the `grpc-timeout`
   header (`1–8 ASCII digits` followed by a unit `H`, `M`, `S`, `m`, `u`,
   `n`); `None` if absent or malformed.
2. `key = FunctionKey::from_proto(…)` → `Status::invalid_argument` on error.
3. `args.len() > max_body_bytes(name)` → `invalid_argument("args are N bytes;
   the cap for {key} is M; pass bulky data as media_ids")`. Args that are not
   valid JSON → `invalid_argument("args must be UTF-8 JSON")`.
4. `hub.invoke(call)`: `NotServable` → `Status::not_found("no configured
   provider may serve {key}")`; `QueueFull` → `Status::resource_exhausted(…)`.
5. Arm a guard: `struct CallGuard { id: Option<CallId>, hub: FunctionHubHandle }`
   whose `Drop` calls `hub.abandon(id)` when `id` is `Some`. This is what
   turns a caller cancel (tonic drops the handler future) into `CancelCall`.
6. `tokio::time::timeout(deadline, receiver)`:
   - `Ok(Ok(Completed{…}))` → disarm (`id.take()`), return `InvokeResponse`.
   - `Ok(Ok(Failed(Provider{code,…})))` → disarm; `InvalidArgument` →
     `Status::invalid_argument`, `Unsupported` → `Status::unimplemented`,
     `Failed` → `Status::internal`; message
     `"{key} failed on provider {id}: {message}"`.
   - `Ok(Ok(Failed(ProviderLost{…})))` → disarm; `Status::unavailable("provider
     {id} disconnected during the call")`.
   - `Ok(Err(_))` → `Status::internal("function hub stopped")`.
   - `Err(_)` (deadline) → disarm; `hub.abandon_and_report(id)`;
     `Status::deadline_exceeded` with the message by report:
     `Queued{provider_connected:false}` → "no provider for {key} connected
     within {deadline:?}"; `Queued{true}` → "{key} stayed queued behind busy
     providers for {deadline:?}"; `InFlight{p}` → "provider {p} did not answer
     {key} within {deadline:?}".
7. Log one `info!` per call with `function = %key, caller, provider,
   outcome, queued_ms, total_ms`.

`FunctionTransport::list_functions` → `hub.list()`.

`ProviderTransport::open_provider_session` (model on
`GrpcConnectorService::open_session`):
1. Read the bearer token from the `authorization` metadata *before*
   `into_inner()`.
2. First frame must be `hello` → else `invalid_argument`.
3. `functions.authenticate(&hello.provider_id, token)` false →
   `Status::unauthenticated("unknown provider or wrong token")`.
4. Create the output channel (sizing above); `hub.register(…)`:
   `Invalid(m)` → `invalid_argument(m)`; `NotPermitted{f}` →
   `permission_denied("provider {id} may not register {f}")`; `Owned{f,o}` →
   `already_exists("{f} is served by provider {o}")`.
5. Spawn a reader: for each later frame, `result` → `hub.result(ticket,
   result)`; a second `hello` or an empty body → `warn!` and stop reading. When
   the stream ends or errors, `hub.closed(ticket)`.
6. Return `ReceiverStream::new(output_rx).map(Ok)`.

**Verify**: `cargo build -p wuphf-functions` → exit 0;
`cargo clippy -p wuphf-functions --all-targets` → no warnings.

### Step 6: Tests

Write `src/tests.rs` (see Test plan for the case list). Harness: a helper
`async fn start(config: FunctionsConfig, tokens: &[(&str, &str)]) -> (String /*uri*/, JoinHandle<()>)`
that builds `Functions::for_tests`, spawns `FunctionHub::run`, and serves
`FunctionTransportServer::new(…)` and `ProviderTransportServer::new(…)` on
`127.0.0.1:0` exactly like `crates/wuphf-runtime/src/tests.rs:486-500`. A
helper `fn provider_request(token, hello_frames…)` builds a
`tonic::Request` with `authorization: Bearer <token>` metadata over an
`mpsc`-fed `ReceiverStream` of `ProviderClientFrame`s, so a test can push
results after reading calls. Use small timings in test configs
(`default_deadline_ms = 300`, `max_deadline_ms = 600`). Never `sleep` to
order events; await frames and replies instead.

**Verify**: `cargo test -p wuphf-functions` → all pass.

Commit 2.

### Step 7: Wire it into the CLI

`crates/wuphf-cli/Cargo.toml`: add `wuphf-functions.workspace = true`.

`crates/wuphf-cli/src/main.rs`:
- `Configuration`: add `#[serde(default)] functions: FunctionsConfig,` with a
  doc comment.
- `serve()`:
  - `let functions = Arc::new(configuration.functions.resolve(|name| std::env::var(name).ok()).context("invalid [functions] configuration")?);`
  - `if functions.invoke_token().is_none() { tracing::warn!("function service has no invoke token set; bind it to a local address only"); }`
  - `let (function_hub, function_handle) = FunctionHub::new(Arc::clone(&functions)); let function_task = tokio::spawn(function_hub.run());`
  - on the `Server::builder()`:
    `.http2_keepalive_interval(Some(functions.keepalive_interval()))`
    `.http2_keepalive_timeout(Some(functions.keepalive_timeout()))`
    (server-wide; it also lets the core notice dead connector connections),
    `.add_service(FunctionTransportServer::with_interceptor(GrpcFunctionService::new(function_handle.clone(), Arc::clone(&functions)), bearer_auth(functions.invoke_token().map(str::to_owned))))`
    `.add_service(ProviderTransportServer::new(GrpcProviderService::new(function_handle, Arc::clone(&functions))))`
  - `function_task.abort()` next to `hub_task.abort()`.
- New subcommand:

  ```rust
  enum Command {
      Serve { … },
      /// Inspect the remote functions registered with a running core.
      Functions {
          #[command(subcommand)]
          command: FunctionsCommand,
      },
  }
  #[derive(Subcommand)]
  enum FunctionsCommand {
      /// List functions with their provider, load and counters. Reads the
      /// invoke token from WUPHF_INVOKE_TOKEN when set.
      List {
          #[arg(long, default_value = "http://127.0.0.1:50051")]
          address: String,
      },
  }
  ```
  `list` connects a `FunctionTransportClient`, attaches `authorization:
  Bearer $WUPHF_INVOKE_TOKEN` when the variable is set (never take the token
  as a flag — it would show in `ps`), and prints one line per function:
  `media.convert@1  provider=mediaconv  in_flight=0/2  queued=0  completed=12  failed=1  abandoned=0`
  (`provider=-` when none).

`config/example.toml`: append a commented-out section:

```toml
# Remote functions (see docs/connector-protocol.md, "Remote functions").
# Providers dial the core and serve named functions; connectors call them.
# A call to a name no provider below may serve fails at once; a call to a
# servable name waits for a provider until its deadline.
# [functions]
# default_deadline_ms = 30000
# max_deadline_ms = 60000
# keepalive_interval_ms = 10000
# keepalive_timeout_ms = 20000
# max_body_bytes = 1048576
# max_queued_calls = 256
# invoke_token_env = "WUPHF_INVOKE_TOKEN"
#
# [[functions.providers]]
# id = "mediaconv"
# token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
# functions = ["media.convert"]
#
# [functions.overrides."media.convert"]
# default_deadline_ms = 20000
```

Add a CLI test `functions_section_parses` that parses a config with an
uncommented `[functions]` section like the one above (one provider, one
override) and asserts the provider id and the override's
`default_deadline_ms`. The existing `example_configuration_parses` must still
pass (the section is commented out there).

**Verify**: `cargo test -p wuphf-cli` → all pass;
`cargo run -p wuphf-cli -- functions list --help` → exit 0, shows `--address`.

Commit 3.

### Step 8: Document

- `README.md` Layout block: add
  `wuphf-functions/ Remote function broker: provider sessions and Invoke`.
- `docs/architecture.md` Components table: add a `wuphf-functions` row
  ("Brokers remote function calls between callers and provider processes;
  independent of the core and storage."), plus a short "Remote functions"
  paragraph stating decisions 2, 3, 6, 7 and 9 in prose.
- `docs/connector-protocol.md`: add a "Remote functions" section: the two
  services, the hello/call/result/cancel frames, naming and majors, the body
  cap and `media_ids`, deadline rules, takeover, the status codes callers can
  see (`NOT_FOUND`, `RESOURCE_EXHAUSTED`, `DEADLINE_EXCEEDED`, `UNAVAILABLE`,
  `INVALID_ARGUMENT`, `UNIMPLEMENTED`, `INTERNAL`), and the config section.
- `docs/index.html`: add a `wuphf-functions` row to the crate table matching
  the existing rows' markup.

**Verify**: `grep -c "wuphf-functions" README.md docs/architecture.md docs/index.html`
→ each ≥ 1; `grep -c "Remote functions" docs/connector-protocol.md` → ≥ 1.

Commit 4.

### Step 9: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0; total passed = Step 0 + the new tests.
`cargo tree -p wuphf-functions --depth 1 | grep -E "wuphf-(core|storage|router|model|runtime)"`
→ no output.

## Test plan

In `crates/wuphf-functions/src/tests.rs` (end-to-end over TCP):

1. `invoke_returns_provider_result` — provider registers `test.echo@1`;
   invoke with `{"x":1}` and one media id; provider sees the same args, media
   ids, caller and a future `deadline_unix_ms`; caller gets the provider's
   result, media ids and `provider_id`.
2. `invoke_waits_for_a_provider_that_registers_later` — invoke first, then
   register; the call completes.
3. `invoke_for_unservable_name_is_not_found_immediately` — no provider pattern
   matches; `NOT_FOUND` returned well before the default deadline.
4. `invoke_without_provider_times_out_with_reason` — servable but nobody
   connected; `DEADLINE_EXCEEDED`, message contains "no provider".
5. `requested_deadline_is_capped_by_max` — client sets a 10 s timeout,
   `max_deadline_ms = 300`; `DEADLINE_EXCEEDED` within ~300 ms.
6. `override_default_deadline_wins_over_provider_suggestion`
   (unit test on `Functions::deadline`, with and without override/suggestion/request).
7. `other_provider_claiming_owned_function_is_rejected` — `ALREADY_EXISTS`.
8. `same_provider_reconnecting_takes_over` — second session with the same id
   succeeds; the first session's stream ends; calls now go to the second.
9. `wrong_token_is_unauthenticated` and
   `function_outside_patterns_is_permission_denied`.
10. `idempotent_call_is_requeued_when_provider_drops` — provider A receives the
    call, drops its stream; provider A' (same id) registers and receives the
    same call id; the caller gets A''s result.
11. `non_idempotent_call_fails_when_provider_drops` — `UNAVAILABLE`.
12. `caller_cancel_sends_cancel_to_provider` — client invokes with a short
    timeout while the provider holds the call; the provider receives
    `CancelCall` with that call id; a late result is ignored and the next
    call still works.
13. `concurrency_limit_queues_extra_calls` — `max_concurrency = 1`, two
    invokes; the provider sees the second call only after answering the first.
14. `full_queue_is_resource_exhausted` — `max_queued_calls = 1`, servable,
    nobody connected; second invoke returns `RESOURCE_EXHAUSTED` immediately.
15. `oversized_or_non_json_args_are_invalid` — both `INVALID_ARGUMENT`.
16. `provider_error_codes_map_to_status` — `INVALID_ARGUMENT`,
    `UNSUPPORTED`→`UNIMPLEMENTED`, `FAILED`→`INTERNAL`.
17. `list_functions_reports_provider_load_and_counters`.
18. Config unit tests: each `resolve` rejection listed in Step 3; pattern
    matching (`*`, `media.*` vs `mediax.y`, exact); name validation.
19. `grpc_timeout_header_parses` — `"100m"`, `"2S"`, `"1H"`, malformed → `None`.

Plus `functions_section_parses` in `wuphf-cli`.

## Done criteria

- [ ] `cargo test --workspace` exits 0; total = baseline + new tests; the
      cases above exist by name
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings;
      `cargo fmt --check` exits 0
- [ ] `cargo tree -p wuphf-functions --depth 1 | grep -E "wuphf-(core|storage|router|model|runtime)"` → no output
- [ ] `git status --short proto/go` showed only the two new generated files
      before commit 1, and `cd proto/go && go vet ./...` exits 0
- [ ] `git diff --stat 2c92f5a..HEAD` lists only in-scope paths
- [ ] `plans/README.md` status row updated

## STOP conditions

- Regenerating the Go bindings modifies any existing `*.pb.go` file (tool
  version drift — report the versions instead of committing churn).
- tonic `0.12` lacks `http2_keepalive_interval`/`http2_keepalive_timeout` on
  `Server`, or dropping the `Invoke` handler future on caller cancel does not
  happen (test 12 cannot pass without sleeping) — report; do not poll.
- Implementing any step seems to require a dependency on `wuphf-core`,
  `wuphf-storage`, `wuphf-model` or `wuphf-runtime` from `wuphf-functions`.
- Making the CLI compile requires changing an existing service or the
  connector protocol.
- A test needs `tokio::time::sleep` to order events (flaky by construction).

## Maintenance notes

- **Deferred, in rough order of likely need**: schema validation (the stored
  `request_schema` makes it a local change in `service.rs` behind a
  per-function flag); per-caller ACLs (needs connector authentication — fold
  into the later session merge below); a metrics exporter (the hub's `Stats`
  are the source); TLS (today every service relies on a local bind);
  constant-time token comparison (`bearer_auth` and `authenticate` both use
  `==`, matching the existing observer).
- **Session merge (owner's "later C")**: one role-based session in which a
  peer is a connector, a provider, or both. When done, `ProviderTransport`
  folds into it and connector sessions gain the per-peer token added here.
- **Core-initiated calls** (a dispatch hook that converts media before the
  connector sees it) were rejected for v1: dispatch planning is synchronous
  and deterministic; adding a remote call needs a new dispatch state. Spike
  before attempting.
- The `abandoned` counter includes both caller cancels and deadlines; split it
  if operators need to tell them apart.
- A provider that keeps working after `CancelCall` can briefly exceed its
  `max_concurrency` (the slot is released at cancel). Provider SDKs must
  honor cancel; plan 033's Go SDK does.
- Plans 026 and 028 edit `crates/wuphf-cli/src/main.rs`; whichever lands
  second rebases trivially (this plan only adds fields, a subcommand and
  service lines). Plan 029 edits workspace `members`; same.
