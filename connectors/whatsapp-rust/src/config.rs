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
    /// Environment variable holding this connector's core token; must match the
    /// core's `[[connectors]].token_env` value for this connector id. Required.
    pub core_token_env: String,
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
    /// Stamp each chat's disappearing-messages timer on what this connector
    /// sends, look a group's timer up when the core does not know it, and
    /// warn the owner when a DM's timer is unknown. Part of the registration,
    /// so it must match `supports_disappearing_timers` in the core's
    /// `[[connectors]]` entry for this connector, or the core refuses it.
    #[serde(default)]
    pub supports_disappearing_timers: bool,
    /// Which WhatsApp client the session registers as. Both pair as an Android
    /// client, so WhatsApp delivers view-once media, which it withholds from a
    /// web companion; they differ only in the account type they match.
    /// Applied at pairing, so a change needs a fresh pairing.
    #[serde(default)]
    pub client_mode: ClientMode,
}

/// The WhatsApp client kind the session registers as.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientMode {
    /// A personal WhatsApp account.
    #[default]
    Android,
    /// A WhatsApp Business (SMB) account, which cannot pair as the personal
    /// client.
    AndroidBusiness,
}

fn default_log_level() -> String {
    "info".into()
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("read config {}: {error}", path.display()))?;
        Self::parse(&contents)
            .map_err(|error| anyhow::anyhow!("parse config {}: {error}", path.display()))
    }

    fn parse(contents: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        core_address = "127.0.0.1:50051"
        connector_id = "whatsapp-rust"
        core_token_env = "TOKEN"
        session_database = "session.db"
    "#;

    #[test]
    fn client_mode_defaults_to_android() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.client_mode, ClientMode::Android);
    }

    #[test]
    fn client_mode_reads_business() {
        let config =
            Config::parse(&format!("{MINIMAL}client_mode = \"android_business\"\n")).unwrap();
        assert_eq!(config.client_mode, ClientMode::AndroidBusiness);
    }

    #[test]
    fn an_unknown_client_mode_is_refused() {
        let error = Config::parse(&format!("{MINIMAL}client_mode = \"web\"\n")).unwrap_err();
        assert!(error.to_string().contains("unknown variant"), "{error}");
    }
}
