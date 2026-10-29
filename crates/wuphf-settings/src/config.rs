//! The configuration types, as the TOML file (and the environment) spell
//! them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use wuphf_functions::FunctionsConfig;
use wuphf_model::{ChatAddress, ChatFilter, ChatRef, ConnectorId, Platform, RuleKind};

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
    /// Routing rules by id (`[rules.<id>]`). Each is its own runtime entry,
    /// `rules.<id>`.
    #[serde(default)]
    pub rules: BTreeMap<String, RuleEntry>,
    /// Chats that receive operational notices from connectors (e.g. a WhatsApp
    /// session ending). A notice is never sent through the connector that
    /// raised it, so listing targets on more than one connector is what keeps
    /// the owner reachable when one platform is down.
    #[serde(default)]
    pub notify: Vec<ChatTarget>,
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

/// A chat in the config file and the settings store, written
/// `connector:chat#thread` (see `ChatRef`'s `Display`). Only config uses this
/// form; stored message data keeps `ChatRef`'s field shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTarget(pub ChatRef);

impl Serialize for ChatTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ChatTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse()
            .map(ChatTarget)
            .map_err(serde::de::Error::custom)
    }
}

/// One routing rule, as written under `[rules.<id>]`. The id is the key.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleEntry {
    /// Lower numbers are tried first when several rules match the same chat;
    /// ties go by id. Only needed when rules overlap.
    #[serde(default)]
    pub priority: i32,
    #[serde(flatten)]
    pub flow: RuleEntryKind,
    /// Present only when an old list-style rule (with its `id` field) was
    /// pasted unchanged. Serde cannot reject unknown fields next to a
    /// flattened one, so `derive` rejects this one by name.
    #[serde(default, skip_serializing)]
    pub(crate) id: Option<serde::de::IgnoredAny>,
}

/// The shape of a rule as the config writes it: [`RuleKind`] with every chat
/// as a `connector:chat#thread` string, and the filter as two optional lists.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum RuleEntryKind {
    OneToOne {
        source: ChatTarget,
        destination: ChatTarget,
    },
    ManyToOne {
        source_connector: ConnectorId,
        destination: ChatTarget,
        /// Only these source chats are mirrored. Excludes `except_chats`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only_chats: Option<Vec<ChatTarget>>,
        /// Every source chat except these is mirrored. Excludes `only_chats`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        except_chats: Option<Vec<ChatTarget>>,
    },
}

impl RuleEntryKind {
    /// The model's form of this rule: each chat split back into its connector
    /// and address, and the filter lists turned into a [`ChatFilter`].
    pub fn into_rule_kind(self, rule_id: &str) -> Result<RuleKind, String> {
        match self {
            RuleEntryKind::OneToOne {
                source,
                destination,
            } => Ok(RuleKind::OneToOne {
                source_connector: source.0.connector_id,
                source_chat: source.0.address,
                destination_connector: destination.0.connector_id,
                destination_chat: destination.0.address,
            }),
            RuleEntryKind::ManyToOne {
                source_connector,
                destination,
                only_chats,
                except_chats,
            } => {
                let filter_chats = |chats: Vec<ChatTarget>| -> Result<Vec<ChatAddress>, String> {
                    chats
                        .into_iter()
                        .map(|chat| {
                            if chat.0.connector_id == source_connector {
                                Ok(chat.0.address)
                            } else {
                                Err(format!(
                                    "rule {rule_id:?}: filter chat {} is not on source connector {}",
                                    chat.0, source_connector.0
                                ))
                            }
                        })
                        .collect()
                };
                let source_chats_filter = match (only_chats, except_chats) {
                    (None, None) => ChatFilter::All,
                    (Some(chats), None) => ChatFilter::Only {
                        chats: filter_chats(chats)?,
                    },
                    (None, Some(chats)) => ChatFilter::Not {
                        chats: filter_chats(chats)?,
                    },
                    (Some(_), Some(_)) => {
                        return Err(format!(
                            "rule {rule_id:?}: use only_chats or except_chats, not both"
                        ));
                    }
                };
                Ok(RuleKind::ManyToOne {
                    source_connector,
                    source_chats_filter,
                    destination_connector: destination.0.connector_id,
                    destination_chat: destination.0.address,
                })
            }
        }
    }
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
    /// Platform user ids of the owner on this connector, as the connector
    /// reports actors (e.g. "704338780" for a Telegram user). Settings
    /// requests a connector makes on behalf of anyone else are refused.
    /// File-only: never a runtime setting, since it decides who may change
    /// the others.
    #[serde(default)]
    pub owner_ids: Vec<String>,
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
