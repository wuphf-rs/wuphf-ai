//! The configuration types, as the TOML file (and the environment) spell
//! them.

use serde::{Deserialize, Serialize, Serializer};
use std::fmt;
use std::net::SocketAddr;
use wuphf_functions::FunctionsConfig;
use wuphf_model::{ChatRef, Platform, Rule};

/// The core configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Configuration {
    pub listen: SocketAddr,
    pub database_url: String,
    #[serde(default = "default_max_media_bytes")]
    pub max_media_bytes: usize,
    #[serde(default = "default_orphan_media_retention_seconds")]
    pub orphan_media_retention_seconds: u64,
    /// Bearer token required by the observer service (the TUI read surface). When
    /// unset the observer is unauthenticated — only safe on a local-only bind.
    #[serde(default)]
    pub observer_token: Option<Secret>,
    #[serde(default)]
    pub connectors: Vec<ConnectorConfiguration>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Chats that receive operational notices from connectors (e.g. a WhatsApp
    /// session ending). A notice is never sent through the connector that
    /// raised it, so listing targets on more than one connector is what keeps
    /// the owner reachable when one platform is down.
    #[serde(default)]
    pub notify: Vec<ChatRef>,
    /// Remote functions: which provider processes may serve which function
    /// names, and the deadlines and limits calls run under.
    #[serde(default)]
    pub functions: FunctionsConfig,
}

fn default_max_media_bytes() -> usize {
    32 * 1024 * 1024
}

fn default_orphan_media_retention_seconds() -> u64 {
    24 * 60 * 60
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConnectorConfiguration {
    pub id: String,
    pub platform: String,
    /// Environment variable holding this connector's token. The connector
    /// presents it as `authorization: Bearer <token>`; required.
    pub token_env: String,
    #[serde(default)]
    pub supports_threads: bool,
    #[serde(default)]
    pub supports_topic_create: bool,
}

/// A secret configuration value. It reads like a plain string, but never
/// comes back out: the settings store answers reads by serializing the
/// configuration, so serializing writes `"<redacted>"`, and `Debug` hides it
/// from logs. [`Secret::expose`] is the only way to the value.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("<redacted>")
    }
}

pub(crate) fn platform(name: &str) -> Result<Platform, String> {
    match name {
        "whatsapp" => Ok(Platform::WhatsApp),
        "telegram" => Ok(Platform::Telegram),
        "discord" => Ok(Platform::Discord),
        "signal" => Ok(Platform::Signal),
        "slack" => Ok(Platform::Slack),
        "matrix" => Ok(Platform::Matrix),
        "instagram" => Ok(Platform::Instagram),
        _ if name.starts_with("other:") => Ok(Platform::Other(name[6..].to_owned())),
        _ => Err(format!("unsupported platform {name}")),
    }
}
