//! Routing topology: how rules select chats and how those resolve to the
//! endpoints, replica sets, and conversations a message fans out across.

use crate::*;
use serde::{Deserialize, Serialize};

/// A messaging platform a connector bridges. `Other` keeps the model open to
/// platforms without a dedicated variant.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    WhatsApp,
    Telegram,
    Discord,
    Signal,
    Slack,
    Matrix,
    Other(String),
}

/// A concrete delivery destination on one connector: a chat, optionally a
/// thread within it. Endpoints are members of a [`ReplicaSet`] and are
/// derived from rules at runtime, never hand-configured. `chat_id`/`thread_id`
/// hold the canonical platform address; other addresses for the same endpoint
/// are reachable through [`Alias`]es.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: EndpointId,
    pub connector_id: ConnectorId,
    pub platform: Platform,
    pub chat_id: String,
    pub thread_id: Option<String>,
}

impl Endpoint {
    pub fn address(&self) -> PlatformAddress {
        PlatformAddress {
            chat_id: self.chat_id.clone(),
            thread_id: self.thread_id.clone(),
        }
    }
}

/// The set of endpoints that mirror each other. A [`Message`] delivered to any
/// member is dispatched to all the others. One replica set backs one
/// [`Conversation`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaSet {
    pub id: ReplicaSetId,
    pub endpoints: Vec<EndpointId>,
}

/// A platform-native chat coordinate. `chat_id` identifies the chat/peer/group;
/// `thread_id` selects a thread/topic within it when the platform supports one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlatformAddress {
    pub chat_id: String,
    pub thread_id: Option<String>,
}

impl PlatformAddress {
    pub fn new(chat_id: impl Into<String>, thread_id: Option<String>) -> Self {
        Self {
            chat_id: chat_id.into(),
            thread_id,
        }
    }
}

/// How one side of a routing rule selects chats.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SideKind {
    /// One fixed chat (optionally one thread within it).
    Single {
        chat_id: String,
        #[serde(default)]
        thread_id: Option<String>,
    },
    /// Every real chat on the connector, filtered by explicit ID lists. Owns
    /// conversation identity.
    Source {
        #[serde(default)]
        include: Vec<String>,
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// One threaded container; one thread is created per source conversation.
    Mirror { container: String },
}

/// One side of a [`Rule`]: a connector plus how that connector selects chats.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSide {
    pub connector: ConnectorId,
    #[serde(flatten)]
    pub kind: SideKind,
}

/// A user-declared mapping binding two or more [`RuleSide`]s. Rules are the only
/// routing input a user writes; conversations, endpoints, and groups are derived
/// from them lazily at runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub sides: Vec<RuleSide>,
}

/// A resolvable platform address for an endpoint. Many aliases may point at one
/// endpoint (e.g. after a WhatsApp phone-to-LID migration); exactly one is
/// canonical for outbound sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    pub connector_id: ConnectorId,
    pub address: PlatformAddress,
    pub endpoint_id: EndpointId,
    pub is_canonical: bool,
}

/// The abstract unit a rule pairs across sides, backed by one replica set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub rule_id: RuleId,
    pub replica_set_id: ReplicaSetId,
    pub source_connector: ConnectorId,
    pub source_address: PlatformAddress,
}

/// Records the mirror thread created for a conversation in a container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMapping {
    pub conversation_id: ConversationId,
    pub connector_id: ConnectorId,
    pub container: String,
    pub thread_id: String,
}
