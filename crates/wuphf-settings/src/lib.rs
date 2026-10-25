//! The core configuration: its types, how it loads (the TOML file, then
//! `WUPHF_CORE_*` environment variables), and the schema the `botconf`
//! settings store validates it with.
//!
//! This crate turns a configuration into what the core runs from (the routing
//! config, the connector list, the resolved functions and connector tokens).
//! Routing decisions themselves stay in `wuphf-core`, which does not depend on
//! this crate, figment or botconf.

mod config;
mod load;
mod schema;
#[cfg(test)]
mod tests;

pub use config::{Configuration, ConnectorConfiguration, Secret};
pub use load::{ENV_PREFIX, LoadError, base, load};
pub use schema::{Derived, SettingsStore, Snapshot, WuphfSchema};
