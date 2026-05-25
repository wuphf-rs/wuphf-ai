//! Core service library for durable logical-message replication.

#![warn(missing_docs)]

/// Runtime configuration and topology validation.
pub mod config;
/// Delivery lease and retry-delay policy implementation.
mod delivery;
/// Application-facing bridge engine operations.
pub mod engine;
/// gRPC connector service adapter.
pub mod grpc;
/// Ordered processing of normalized inbound operations.
mod pipeline;
/// Durable persistence transactions for bridge state.
mod repository;
/// Precomputed endpoint ownership and replication targets.
mod routing;
/// Active connector stream session tracking.
mod sessions;
/// SQLite pool setup, migrations, and configured topology storage.
pub mod store;

/// Validated core runtime configuration.
pub use config::Config;
/// Core application facade and operation errors.
pub use engine::{Engine, EngineError};
/// Tonic service implementation for connector streams.
pub use grpc::ConnectorService;
/// Initialized SQLite-backed store.
pub use store::Store;
