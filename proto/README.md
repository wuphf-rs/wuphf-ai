# Protocol Definitions

This directory contains `connector.proto`, the canonical protocol definition,
and the `wuphf-proto` Rust bindings crate. The `.proto` contract remains usable
by platform clients in any implementation language.

Protocol changes must preserve compatibility deliberately and remain aligned
with [`../docs/protocol.md`](../docs/protocol.md).
