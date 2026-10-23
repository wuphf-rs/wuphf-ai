//! Remote function broker. Provider processes in any language dial the core
//! over `ProviderTransport`, register named, versioned functions (`dotted.name`
//! plus a major version) they are configured to serve, and answer calls; any
//! caller invokes them by name through `FunctionTransport`. The core only
//! brokers: every call goes caller → core → provider → core → caller, bodies
//! are small JSON with bulky data passed as media ids, and nothing is persisted.
//!
//! A call to a name some configured provider may serve waits for that provider
//! until its deadline; a name no provider may serve fails at once. A provider
//! reconnecting with the same id takes over its old session, and calls lost with
//! a session are re-queued when the function is idempotent, failed otherwise.
//! This crate deliberately depends on neither the core nor storage, so routing
//! and delivery never depend on a provider being up.

mod config;
mod hub;
mod service;
#[cfg(test)]
mod tests;

pub use config::{
    ConfigError, FunctionKey, FunctionOverride, Functions, FunctionsConfig, ProviderConfig,
};
pub use hub::{FunctionHub, FunctionHubHandle};
pub use service::{GrpcFunctionService, GrpcProviderService};
