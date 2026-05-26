# Development Workflow

## Current Status

The first MVP is implemented as a Rust core service, canonical protobuf
contract, SQLite migration, and Rust mock connector. It demonstrates the
replication model without completed WhatsApp or Telegram support. Real
WhatsApp to Telegram operation and Telegram topic mapping are the next
platform milestone.

## Development Environment

Development uses the repository `flake.nix` and an `.envrc` file containing:

```sh
use flake
```

With direnv installed and integrated into the user's shell, allow the checkout:

```sh
direnv allow
```

Entering the directory will then load the Nix development shell automatically.
The development shell includes:

- Rust and Cargo.
- `rust-analyzer`.
- `rustfmt` and `clippy`.
- A protobuf compiler required by Tonic/Prost builds.
- SQLite and `pkg-config`.

## Repository Layout

The source layout separates server code from the wire contract and
client implementations:

- `core/`: the Rust server implementation and any Rust-only generated bindings
  or support crates.
- `proto/`: canonical protobuf definitions usable by every client language.
- `clients/`: platform clients, including `clients/mock/` for deterministic
  protocol and integration testing. Clients may use any language with gRPC
  support.
- `config/`: checked-in example configuration.
- `migrations/`: SQLite migrations owned by the core.
- `tests/integration/`: core-to-client behavior tests.

The core implementation stack is Rust with Tokio, Tonic/Prost,
Serde/TOML, SQLx/SQLite, UUIDs, and `tracing`. Platform client dependency
choices are language-specific and must conform to the shared gRPC contract.

## Configuration

Runtime configuration is supplied through TOML. The example configuration at
[`../config/example.toml`](../config/example.toml) describes two linked mock
rooms. The configuration surface describes:

- Server settings.
- SQLite database settings.
- Configured connector instances and their authentication tokens.
- Endpoints owned by each connector instance.
- Replication groups that define routing membership.

The core, not connector instances, interprets routing membership and owns
logical message, message copy, dispatch command, deduplication, and retry
state.

## Run The MVP

Start the core in one shell:

```sh
cargo run -p wuphf-core -- --config config/example.toml
```

Connect a receiving mock endpoint in another shell:

```sh
cargo run -p wuphf-mock -- \
  --connector mock-b --token development-token-b --endpoint room-b \
  --ack-platform-id b-parent
```

Submit a message from `room-a`; the receiver prints its dispatch and
acknowledges a generated target platform ID:

```sh
cargo run -p wuphf-mock -- \
  --connector mock-a --token development-token-a --endpoint room-a \
  --send 'hello from room-a' --platform-message-id a-parent
```

The inverse command with `mock-b` and `room-b` demonstrates delivery in the
other direction. To reply from `room-a`, submit a new `--send` from `mock-a`
with `--reply-to a-parent`; the `room-b` dispatch contains `b-parent`, the
native parent ID its connector acknowledged. A reply originating at `room-b`
instead uses `b-parent` as its `--reply-to`.

For durable replay, start a receiver with `--no-ack`, deliver a message to it,
stop that receiver, and reconnect it without `--no-ack`. The unfinished
dispatch is loaded from SQLite and delivered again.

For retry behavior, run the receiver with `--fail-reason temporary`; the core
records a failed attempt and redelivers it after the configured retry delay.
Adding `--permanent-failure` records terminal failure and stops automatic
delivery for that endpoint sequence.

Inspect terminal dispatches through the core-owned operator command:

```sh
cargo run -p wuphf-core -- --config config/example.toml \
  dispatches list --terminal-only
```

The list includes attempt count, retry eligibility timestamp, and last
reported failure. It returns at most 100 records by default; pass `--limit`
up to `1000` for a larger diagnostic page. To reopen one terminal dispatch,
stop the serving core process if it is using this SQLite file and run:

```sh
cargo run -p wuphf-core -- --config config/example.toml \
  dispatches retry <dispatch-id>
```

Reconnect the target mock connector to reclaim the reopened command
immediately. The command changes durable state through the core repository;
do not update SQLite tables manually.

## Persistence And Protocol Discipline

SQLite is part of the first MVP so logical-message identity mappings and
durable pending dispatch work remain correct after reconnects and restarts.
At startup the core opens SQLite with foreign-key enforcement, WAL journaling,
full synchronous durability, and a five-second busy timeout. It logs applied
migration version and configured topology counts without logging connector
tokens.
The `[delivery]` section in the TOML configuration sets acknowledgement
timeout, initial retry delay, and maximum exponential retry delay.
Changes to persistent state require migrations. Changes to protobuf contracts
must preserve compatibility deliberately rather than silently changing the
wire contract.

The serving core handles Ctrl-C and SIGTERM through Tonic graceful shutdown,
stopping new stream acceptance while active connections close.

New implementation must retain the invariants described in
[System Architecture](architecture.md) and
[Connector Protocol](protocol.md), particularly idempotency, loop prevention,
per-endpoint dispatch ordering, and blocking reply dispatch until the
target-side parent message copy exists.

## Verification Gates

Changes should pass:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
nix flake check
```
