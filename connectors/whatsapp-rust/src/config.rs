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
    /// The account's phone number (country code, digits only), which a pair
    /// code is issued for. Required by `login_method = "pair_code"`.
    #[serde(default)]
    pub phone_number: Option<String>,
    /// How the session is linked, at first start and again after a logout:
    /// `"qr"` (scan a QR code) or `"pair_code"` (type an 8-character code into
    /// WhatsApp). Unset: `pair_code` when `phone_number` is set, else `qr`.
    #[serde(default)]
    login_method: Option<LoginMethod>,
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
    /// Settings that may change while the connector runs (plan 056 makes them
    /// live; until then they are read once at startup). Flattened, so the file
    /// keeps its layout (`[rendering]`).
    #[serde(flatten)]
    pub runtime: Runtime,
}

/// Settings that may change while the connector runs, the shape of the Go
/// connector's `config.Runtime`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Runtime {
    #[serde(default)]
    pub rendering: Rendering,
}

/// How mirrored copies are rendered on WhatsApp: this connector's own policy;
/// the core dispatches content only.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Rendering {
    /// Send every attachment as a file, whatever its kind.
    #[serde(default)]
    pub attachments_as_files: bool,
    /// Longer text (in characters) is sent as a plain-text file; 0 means no
    /// limit.
    #[serde(default)]
    pub max_message_length: u32,
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

/// How the WhatsApp session is linked to the account.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    /// Scan a QR code in WhatsApp → Linked devices → Link a device.
    Qr,
    /// Type an 8-character code into WhatsApp after "Link with phone number
    /// instead"; needs `phone_number`.
    PairCode,
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

    fn parse(contents: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(contents)?;
        if config.login_method() == LoginMethod::PairCode && config.phone_number.is_none() {
            anyhow::bail!("login_method = \"pair_code\" needs phone_number");
        }
        Ok(config)
    }

    /// The configured login method, defaulted from `phone_number`: before the
    /// key existed, a set phone number meant pairing by code.
    pub fn login_method(&self) -> LoginMethod {
        self.login_method.unwrap_or(match self.phone_number {
            Some(_) => LoginMethod::PairCode,
            None => LoginMethod::Qr,
        })
    }

    /// The phone number a pair code is issued for, when the session pairs by
    /// code.
    pub fn pair_code_phone(&self) -> Option<&str> {
        match self.login_method() {
            LoginMethod::PairCode => self.phone_number.as_deref(),
            LoginMethod::Qr => None,
        }
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
    fn the_login_method_defaults_from_the_phone_number() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.login_method(), LoginMethod::Qr);
        assert_eq!(config.pair_code_phone(), None);
        let config = Config::parse(&format!("{MINIMAL}phone_number = \"15551234567\"\n")).unwrap();
        assert_eq!(config.login_method(), LoginMethod::PairCode);
        assert_eq!(config.pair_code_phone(), Some("15551234567"));
    }

    #[test]
    fn qr_login_ignores_a_phone_number() {
        let config = Config::parse(&format!(
            "{MINIMAL}phone_number = \"15551234567\"\nlogin_method = \"qr\"\n"
        ))
        .unwrap();
        assert_eq!(config.login_method(), LoginMethod::Qr);
        assert_eq!(config.pair_code_phone(), None);
    }

    #[test]
    fn pair_code_login_needs_a_phone_number() {
        let error = Config::parse(&format!("{MINIMAL}login_method = \"pair_code\"\n")).unwrap_err();
        assert!(error.to_string().contains("needs phone_number"), "{error}");
    }

    #[test]
    fn rendering_defaults_off_and_reads_its_section() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.runtime, Runtime::default());
        let config = Config::parse(&format!(
            "{MINIMAL}[rendering]\nattachments_as_files = true\nmax_message_length = 4096\n"
        ))
        .unwrap();
        assert!(config.runtime.rendering.attachments_as_files);
        assert_eq!(config.runtime.rendering.max_message_length, 4096);
    }

    #[test]
    fn an_unknown_client_mode_is_refused() {
        let error = Config::parse(&format!("{MINIMAL}client_mode = \"web\"\n")).unwrap_err();
        assert!(error.to_string().contains("unknown variant"), "{error}");
    }
}
