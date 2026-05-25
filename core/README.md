# Core Server

This directory contains the Rust `wuphf` core server crate. The core owns
routing, authentication, SQLite persistence,
logical message and message copy identity, durable dispatch commands,
deduplication, retry behavior, and reply dependencies.

Platform clients communicate with this server through the gRPC definitions in
[`../proto/`](../proto/); they are not required to be written in Rust.
