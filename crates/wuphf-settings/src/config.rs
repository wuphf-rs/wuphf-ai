//! The configuration types, as the TOML file (and the environment) spell
//! them.

use botconf::Describe;
use botconf::describe::Description;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use wuphf_functions::FunctionsConfig;
use wuphf_model::{ChatAddress, ChatFilter, ChatRef, ChatStateDirection, ConnectorId, RuleKind};

/// The core configuration file's description: every option, from the types
/// below. The checked-in example (`config/example.toml`) is its example.
pub fn description() -> Description {
    Description::of::<Configuration>("wuphf core", FILE_DESCRIPTION)
}

const FILE_DESCRIPTION: &str = "Every key can be overridden by an environment variable: \
     `WUPHF_CORE_` plus the key path in upper case, with `__` between levels \
     (`WUPHF_CORE_LISTEN`, `WUPHF_CORE_FUNCTIONS__MAX_DEADLINE_MS`).\n\n\
     A chat is written as one string, `connector:chat`, optionally `#thread` \
     (`telegram-main:telegram:channel:2060340733#12`). The connector is everything before \
     the first `:`, so chat ids may contain `:`. Inside the chat and the thread, write `#` \
     as `%23` and `%` as `%25`. Connector ids may not contain `:`, `#`, `%` or spaces.\n\n\
     Runtime settings (`notify`, `rules`, `chat_state`) can also be changed while the core \
     runs, from a settings panel or `wuphf-server settings`: stored overrides win over this \
     file, and unsetting one goes back to it.";

/// The core configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Describe)]
pub struct Configuration {
    /// The address the core's gRPC services listen on (connectors, providers,
    /// the observer, the settings service).
    #[botconf(example = "127.0.0.1:50051")]
    pub listen: SocketAddr,
    /// The SQLite database holding messages, media and the runtime settings'
    /// overrides.
    #[botconf(example = "sqlite://wuphf.sqlite")]
    pub database_url: String,
    /// The largest media file the core accepts, in bytes.
    #[serde(default = "default_max_media_bytes")]
    pub max_media_bytes: usize,
    /// How long media no message refers to is kept, in seconds.
    #[serde(default = "default_orphan_media_retention_seconds")]
    pub orphan_media_retention_seconds: u64,
    /// Token the observer service (the TUI read surface) requires as
    /// `authorization: Bearer <token>`.
    ///
    /// Leave it unset only when `listen` is bound to a local address: without
    /// it the observer, which can read every captured chat, is
    /// unauthenticated.
    #[serde(default)]
    pub observer_token: Option<Secret>,
    /// The connectors the core accepts.
    ///
    /// Connectors are thin pipes. The core only trusts a session that presents
    /// the connector's token (`authorization: Bearer <token>`, read from the
    /// environment variable named by `token_env`) and whose registration
    /// matches an entry here. The core refuses to start while a connector's
    /// variable is unset or empty: list a connector only when you run it. A
    /// second session for a connector whose session is still live is refused
    /// (ALREADY_EXISTS); the connector keeps retrying and gets in once the old
    /// session has ended. Media upload and download accept any connector's or
    /// function provider's token.
    ///
    /// How a connector renders replicated copies (attachments as files, text
    /// spillover, edit and delete rendering) is configured in each
    /// connector's own config file under `[rendering]`, not here.
    #[serde(default)]
    #[botconf(example = [
        {
            "id": "whatsapp-main",
            "platform": "whatsapp",
            "token_env": "WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN",
            "supports_disappearing_timers": true,
        },
        {
            "id": "instagram-main",
            "platform": "instagram",
            "token_env": "WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN",
        },
        {
            "id": "telegram-main",
            "platform": "telegram",
            "token_env": "WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN",
            "supports_threads": true,
            "supports_topic_create": true,
        },
    ])]
    pub connectors: Vec<ConnectorConfiguration>,
    /// Routing rules by id: which chats mirror each other. Lower priority
    /// numbers match first.
    ///
    /// Each rule is its own runtime entry (`rules.<id>`). A rule's id is how
    /// its conversations remember it: renaming a rule (changing its key)
    /// starts new mirror threads.
    ///
    /// Rules declare a flow between a source and a destination. The source end
    /// owns conversation identity; replication itself is two-way. When several
    /// rules match the same chat, the one with the lowest `priority` (default
    /// 0) wins, then the lowest id. Set it only when rules overlap, e.g.
    /// `priority = -1` on a one-to-one exception to a many-to-one catch-all.
    ///
    /// Chat ids may be written in any address form the platform uses for the
    /// chat. WhatsApp reaches a contact as both a phone JID
    /// (`<number>@s.whatsapp.net`) and a hidden LID (`<number>@lid`), and
    /// delivers messages under either; a list entry written with one form
    /// selects (or, in a blocklist, excludes) the chat under all of them, as
    /// soon as the connector has reported that the forms belong together.
    /// Group ids (`<id>@g.us`) have a single form.
    #[serde(default)]
    #[botconf(
        per_entry,
        panel = crate::schema::RULE.clone(),
        example = {
            "wa-to-telegram-mirror": {
                "kind": "many-to-one",
                "source_connector": "whatsapp-main",
                "destination": "telegram-main:telegram:channel:2060340733",
            },
            "ig-to-telegram-mirror": {
                "kind": "many-to-one",
                "source_connector": "instagram-main",
                "destination": "telegram-main:telegram:channel:2060340733",
            },
        },
    )]
    pub rules: BTreeMap<String, RuleEntry>,
    /// Chats that receive notices about connector trouble.
    ///
    /// A connector that loses its session stops bridging silently (from the
    /// outside that looks exactly like nobody writing to you), so it tells the
    /// core, which relays a message here. A notice is never sent through the
    /// connector that raised it: a logged-out WhatsApp cannot deliver the news
    /// that it is logged out. List a target on each platform you want to stay
    /// reachable on, and leave this out to keep notices in the log only.
    #[serde(default)]
    #[botconf(
        runtime,
        title = "Notice targets",
        panel = Kind::reference(crate::domains::CHAT, true),
        example = ["telegram-main:telegram:user:704338780"],
    )]
    pub notify: Vec<ChatTarget>,
    /// Pin, mute and archive sync between a source chat and its mirror topic
    /// (Telegram: a pinned topic, the topic's notifications, a closed topic).
    /// Muting a topic needs a Telegram user session (a bot's notify settings
    /// mean nothing).
    #[serde(default)]
    pub chat_state: ChatStateConfig,
    #[serde(default)]
    pub functions: FunctionsConfig,
    // Present only when the file holds a `[connector_settings]` table, which
    // `derive` refuses: a connector's settings take their base from the
    // connector's own file, so a second base here would make unsetting an
    // override ambiguous.
    #[serde(default, skip_serializing)]
    #[botconf(skip)]
    pub(crate) connector_settings: Option<serde::de::IgnoredAny>,
}

fn default_max_media_bytes() -> usize {
    32 * 1024 * 1024
}

fn default_orphan_media_retention_seconds() -> u64 {
    24 * 60 * 60
}

/// `[chat_state]`: each state's direction, named from the source chat
/// (`to_mirror`: source → mirror, `to_source`: mirror → source), and
/// per-conversation overrides.
#[derive(Debug, Clone, Default, Deserialize, Serialize, Describe)]
pub struct ChatStateConfig {
    /// Which way pinning crosses between a chat and its mirror topic.
    #[serde(default)]
    #[botconf(runtime, title = "Pin", panel = crate::schema::DIRECTION)]
    pub pin: ChatStateDirection,
    /// Which way muting crosses between a chat and its mirror topic.
    #[serde(default)]
    #[botconf(runtime, title = "Mute", panel = crate::schema::DIRECTION)]
    pub mute: ChatStateDirection,
    /// Which way archiving (closing the topic) crosses between a chat and its
    /// mirror topic.
    #[serde(default)]
    #[botconf(runtime, title = "Archive", panel = crate::schema::DIRECTION)]
    pub archive: ChatStateDirection,
    /// Per-conversation overrides of chat_state; the chat is the source chat
    /// or its mirror topic.
    ///
    /// An override's name is free (`[chat_state.chats.family]`); it cannot be
    /// the chat id, which may contain `.`. An absent state takes the global
    /// value.
    #[serde(default)]
    #[botconf(
        per_entry,
        title = "Chat state overrides",
        panel = crate::schema::CHAT_STATE_OVERRIDE.clone(),
    )]
    pub chats: BTreeMap<String, ChatStateOverride>,
}

/// One conversation's override, named by `chat`: its source chat or its
/// mirror topic. An absent state takes the global value.
#[derive(Debug, Clone, Deserialize, Serialize, Describe)]
pub struct ChatStateOverride {
    /// The source chat or its mirror topic.
    pub chat: ChatTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<ChatStateDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<ChatStateDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ChatStateDirection>,
}

/// A chat, written `connector:chat`, optionally `#thread`.
// The config file and the settings store's form (see `ChatRef`'s `Display`);
// stored message data keeps `ChatRef`'s field shape.
#[derive(Debug, Clone, PartialEq, Eq, Describe)]
#[botconf(kind = ValueKind::Text)]
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
#[derive(Debug, Clone, Deserialize, Serialize, Describe)]
pub struct RuleEntry {
    /// Lower numbers are tried first when several rules match the same chat;
    /// ties go by id. Only needed when rules overlap.
    #[serde(default)]
    pub priority: i32,
    #[serde(flatten)]
    pub flow: RuleEntryKind,
    // Present only when an old list-style rule (with its `id` field) was
    // pasted unchanged. Serde cannot reject unknown fields next to a
    // flattened one, so `derive` rejects this one by name.
    #[serde(default, skip_serializing)]
    #[botconf(skip)]
    pub(crate) id: Option<serde::de::IgnoredAny>,
}

/// The shape of a rule as the config writes it: [`RuleKind`] with every chat
/// as a `connector:chat#thread` string, and the filter as two optional lists.
#[derive(Debug, Clone, Deserialize, Serialize, Describe)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum RuleEntryKind {
    /// One fixed source chat mirrored into one fixed destination chat
    /// (optionally a thread within it).
    #[botconf(title = "One chat to one chat")]
    OneToOne {
        source: ChatTarget,
        destination: ChatTarget,
    },
    /// Every chat on the source connector, each mirrored into its own topic
    /// auto-created inside the destination container chat. It may limit its
    /// source chats with `only_chats` (an allowlist) or `except_chats` (a
    /// blocklist), not both; the chats must be on `source_connector`.
    #[botconf(title = "Every chat into one, a thread each")]
    ManyToOne {
        /// The connector whose chats are mirrored.
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

/// One connector the core accepts.
#[derive(Debug, Clone, Deserialize, Serialize, Describe)]
pub struct ConnectorConfiguration {
    /// The connector's id, as its own config names it (`connector_id`).
    pub id: String,
    /// `whatsapp`, `telegram`, `instagram`, `discord`, `signal`, `slack`,
    /// `matrix`, or `other:<name>`.
    pub platform: String,
    /// Environment variable holding this connector's token. The connector
    /// presents it as `authorization: Bearer <token>`; its own config names
    /// the same variable (`core_token_env`).
    #[botconf(env = token)]
    pub token_env: String,
    /// The connector can post into threads (topics). A many-to-one
    /// destination needs it and `supports_topic_create`.
    #[serde(default)]
    pub supports_threads: bool,
    /// The connector can create threads (topics).
    #[serde(default)]
    pub supports_topic_create: bool,
    /// The core stamps a chat's disappearing-messages timer on sends to this
    /// connector and the connector reports the timers it observes. Part of
    /// the registration, so it must match the connector's own config.
    #[serde(default)]
    pub supports_disappearing_timers: bool,
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
#[derive(Clone, Deserialize, Describe)]
#[serde(transparent)]
#[botconf(secret)]
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
