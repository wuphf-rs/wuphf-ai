//! The core configuration: its types, how it loads (the TOML file, then
//! `WUPHF_CORE_*` environment variables), and the schema the `botconf`
//! settings store validates it with.
//!
//! This crate turns a configuration into what the core runs from (the routing
//! config, the connector list, the resolved functions and connector tokens).
//! Routing decisions themselves stay in `wuphf-core`, which does not depend on
//! this crate, figment or botconf.

mod config;
pub mod domains;
mod load;
mod routing;
mod schema;
mod storage;
#[cfg(test)]
mod tests;
mod tokens;

pub use config::{
    ChatTarget, Configuration, ConnectorConfiguration, RuleEntry, RuleEntryKind, Secret,
    description,
};
pub use load::{ENV_PREFIX, LoadError, base, check, load, source};
pub use routing::LiveRouting;
pub use schema::{CONNECTOR_SETTINGS, Derived, SettingsStore, Snapshot, WuphfSchema};
pub use storage::SettingsStorage;
pub use tokens::PanelTokenStore;
