# wuphf

`wuphf` is a durable messaging replication engine intended to replace
`watgbridge`. Rather than blindly forwarding messages, it records a logical
message and the platform-specific message copies created at each endpoint.
That model supports correct replies, retry, deduplication, ordering, and loop
prevention.

The first MVP is runnable with mock connectors. It includes a Rust core
service, a language-neutral gRPC/protobuf contract, SQLite persistence, TOML
topology configuration, and a Rust mock connector CLI. Real WhatsApp and
Telegram integration, including Telegram topic behavior, remains the next
platform milestone.

## Model

- A **connector instance** authenticates to the core over one long-lived
  bidirectional gRPC stream and owns its configured endpoints.
- An **endpoint** is an addressable chat or mock destination controlled by a
  connector instance.
- A **replication group** determines which endpoints receive replicated
  logical messages.
- A **logical message** is core-owned message identity independent of any
  platform ID.
- A **message copy** links a logical message to an endpoint-native platform
  message ID.
- A **dispatch command** is durable core-owned work delivered to a connector.

The core owns routing, persistence, authentication, message-copy mapping,
deduplication, acknowledgement handling, retry, ordering, reply dependencies,
and loop prevention. Connectors only normalize inbound events and execute
dispatch commands.

## First MVP

Implemented behavior:

- Authenticated connector registration using configured IDs and shared tokens.
- Normalized create-message ingestion and durable create-message dispatch.
- SQLite-backed logical messages, message copies, event deduplication, pending
  dispatches, and acknowledgement outcomes.
- Redelivery of unfinished work after reconnects or core restarts.
- Durable attempt accounting with configurable acknowledgement timeouts and
  exponential retry backoff for transient dispatch failures.
- Explicit permanent dispatch failure reporting, which leaves the endpoint
  sequence blocked for operator investigation rather than silently dropping
  later ordered work.
- Deterministic per-endpoint dispatch order.
- Idempotent event submission and acknowledgement processing.
- Reply dispatch blocking until the parent message has a target-side
  acknowledged copy, followed by target-native reply ID mapping.
- Attachment metadata descriptors only; no media byte transfer or storage.
- A mock connector CLI for bidirectional delivery, reply, and replay
  demonstrations.

Deferred work:

- Production WhatsApp, Telegram, Discord, or other platform connectors.
- Telegram topic mapping.
- Message edits, reactions, deletes, or revokes.
- Media transport or storage.
- Management API or TUI, dynamic replication administration, mTLS, and
  multiple sessions for one connector integration.

## Repository Layout

```text
.
|-- core/                 # Rust gRPC core service
|-- proto/                # Canonical protobuf contract and Rust bindings crate
|-- clients/
|   `-- mock/             # Rust mock connector CLI
|-- config/               # Checked-in example TOML topology
|-- migrations/           # SQLx SQLite migrations
|-- docs/                 # Architecture, protocol, and development workflow
`-- tests/                 # Integration-test documentation
```

## Run The MVP

Enter the Nix development shell through direnv:

```sh
direnv allow
```

Start the core with the two-mock-endpoint example:

```sh
cargo run -p wuphf-core -- --config config/example.toml
```

In a second shell, connect the receiving mock endpoint:

```sh
cargo run -p wuphf-mock -- \
  --connector mock-b --token development-token-b --endpoint room-b \
  --ack-platform-id b-parent
```

In a third shell, submit a synthetic message:

```sh
cargo run -p wuphf-mock -- \
  --connector mock-a --token development-token-a --endpoint room-a \
  --send 'hello from room-a' --platform-message-id a-parent
```

See [Development Workflow](docs/development.md) for reply mapping and durable
replay/failure walkthroughs. The protocol and persistence invariants are described in
[Connector Protocol](docs/protocol.md) and
[System Architecture](docs/architecture.md).

## Verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
nix flake check
```
