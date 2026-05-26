# Connector Protocol

## Session Boundary

A connector opens one session for its platform integration and registers:

- stable connector ID
- platform
- owned endpoint IDs

One session may own multiple endpoints for one platform, including Telegram
topic endpoints. Multiple sessions per platform are deferred.

Registration must match core configuration: connector ID, platform, and owned
endpoint IDs. A connector cannot claim endpoints not assigned to it.

## Connector Input

Connectors send:

- `Event(NormalizedEvent)` for platform-origin input.
- `Acknowledgement(DispatchAcknowledgement)` after successfully creating a
  platform message copy.

The runtime rejects events from endpoints outside the registered session and
acknowledgements for dispatches not assigned to that session.

## Connector Output

The core sends:

- `Dispatch(StoredDispatch)` for pending platform work.
- `Rejected { reason }` when connector input violates session ownership.

`StoredDispatch.id` must be echoed in acknowledgements. The connector supplies
only the resulting native platform message ID; logical identity remains owned
by the core.

Dispatches remain pending until acknowledgement. If a connector reconnects
before acknowledging assigned work, the hub redelivers that work with the
same dispatch ID.

## Transport

Canonical network contract is `crates/wuphf-proto/connector.proto`. It
exposes one bidirectional gRPC `OpenSession` stream. First client frame must
be `Registration`; following frames carry message creates or
acknowledgements. Server frames carry dispatches or rejection results.

`wuphf-runtime::grpc` translates protobuf frames to internal connector hub.
Routing and identity remain outside transport adapter.

The executable server loads configured endpoint and connector assignments from
TOML:

```sh
cargo run -p wuphf-cli -- serve --config config/example.toml
```
