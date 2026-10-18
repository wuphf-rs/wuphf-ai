use std::path::Path;

use serde::Deserialize;

/// Connector-local configuration. Routing is owned entirely by the core; this
/// file only carries how to reach the core and how to run the WhatsApp
/// session.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// The core's gRPC address.
    pub core_address: String,
    /// Must match a `[[connectors]]` entry in the core's configuration.
    pub connector_id: String,
    /// Path of the SQLite database holding the WhatsApp device session.
    /// Reopening the same path restores the paired session.
    pub session_database: String,
    /// Log verbosity (trace, debug, info, warn, error); the WUPHF_LOG_LEVEL
    /// environment variable overrides it.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// When set, pair with an 8-character code for this phone number
    /// (country code, digits only) instead of requiring a QR scan.
    #[serde(default)]
    pub phone_number: Option<String>,
}

fn default_log_level() -> String {
    "info".into()
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("read config {}: {error}", path.display()))?;
        toml::from_str(&contents)
            .map_err(|error| anyhow::anyhow!("parse config {}: {error}", path.display()))
    }
}
