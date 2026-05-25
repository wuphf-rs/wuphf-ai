# Platform Clients

This directory contains gRPC platform clients, also referred to by the
architecture as connector instances. Each client integrates with one platform
or test environment, normalizes events, and executes dispatch commands from
the Rust core.

Clients may be implemented in any language with appropriate gRPC support.
Routing, persistence, identity mapping, deduplication, and retry policy remain
core responsibilities.
