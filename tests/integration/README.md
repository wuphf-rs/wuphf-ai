# Integration Tests

End-to-end protocol coverage between the Rust core server and mock connector
streams is implemented in [`../../core/tests/grpc.rs`](../../core/tests/grpc.rs).
It covers authenticated streams, durable reconnect replay, and reply mapping;
core unit tests cover deduplication, ordering, and loop prevention state.
