# wuphf Contributor Instructions

## Purpose And Scope

`wuphf` is a multi-platform messaging replication engine intended to replace
the predecessor project, `watgbridge`. It models a conversation as logical
messages replicated into message copies at platform endpoints, rather than as
blind message forwarding.

The implemented current milestone is the **first MVP**, backed by mock
connectors only:

- Build a Rust core service and its language-neutral gRPC connector contract.
- Build a mock platform client for development and tests; its implementation
  language is not constrained.
- Do not implement real Telegram, WhatsApp, Discord, or other platform
  connectors in this milestone.

## Architecture Boundaries

- A **connector instance** integrates with one platform or mock environment. It
  normalizes incoming platform events into protocol operations and executes
  dispatch commands sent by the core.
- The **core** owns replication group routing, endpoint configuration, logical
  message and message copy identity mapping, deduplication, persistence,
  dispatch scheduling, acknowledgements, and retries.
- Bridge behavior must not move into connectors. Connectors must not decide
  replication membership, fan-out targets, retry policy, or cross-platform
  identity mappings.
- Persistence must retain enough core state to resume correct dispatch and
  acknowledgement processing after restarts.

## Chosen Stack

Use Rust for the core server implementation with:

- Tokio for async execution.
- Tonic and Prost for gRPC and protobuf types.
- Serde and TOML for configuration.
- SQLx with SQLite for persistent state.
- UUIDs for internal identifiers where identifiers are required.
- `tracing` for structured diagnostics.

Platform clients are separate gRPC consumers and may be implemented in any
language appropriate to their platform SDK and deployment needs. The canonical
wire contract belongs in `proto/`, independent of generated Rust bindings.

## Required V1 Behavior

Implementations in the first MVP must preserve these requirements:

- Connectors use one long-lived bidirectional gRPC stream to the core.
- A connector instance authenticates during registration with its configured
  connector ID and shared token.
- SQLite durably stores dispatch state and dispatch acknowledgement outcomes so
  work can be retried after disconnects or restarts.
- The core models each replicated event as a logical message with platform
  message copies at endpoints.
- A reply dispatch command for a target endpoint remains blocked until the
  parent logical message has an acknowledged message copy at that endpoint.
- Attachments are normalized as metadata descriptors only. V1 does not send,
  store, proxy, or fetch media bytes.
- Ingestion, dispatch, and acknowledgement handling must be idempotent and
  must prevent replicated copies from re-entering the bridge as new logical
  messages.

## Deferred Scope

Do not pull these features into the first MVP unless the project scope
is explicitly changed:

- Production Telegram, WhatsApp, or Discord integrations.
- Message edits, reactions, deletes, or revokes.
- A TUI or management API.
- Dynamic replication group administration.
- mTLS.
- Multiple sessions for a connector integration.

## Development Rules

- Provide and use a Nix flake development shell loaded through direnv.
- Keep server implementation under `core/`, canonical protobuf definitions
  under `proto/`, and platform client implementations under `clients/`.
- Keep terminology aligned with the design documents: **logical message**,
  **message copy**, **endpoint**, **replication group**, **connector
  instance**, and **dispatch command**.
- Treat protobuf evolution as a compatibility concern. Do not make accidental
  breaking changes to messages or operations.
- Add migrations for every persistent model change; do not depend on ad hoc
  schema creation or manual database edits.
- Preserve idempotency, durable retry, ordering, and loop prevention
  invariants in new code and tests.
- Keep connectors thin and platform-facing; implement bridge decisions in the
  core.

Before completing implementation work, run the applicable formatting, linting,
tests, and flake validation checks. The planned baseline commands are:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
nix flake check
```

The architecture and protocol expectations are documented in
[`docs/architecture.md`](docs/architecture.md) and
[`docs/protocol.md`](docs/protocol.md). The planned local workflow is documented
in [`docs/development.md`](docs/development.md).
