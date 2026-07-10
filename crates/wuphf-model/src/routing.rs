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
    Instagram,
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

/// A user-declared replication mapping. Rules are the only routing input a user
/// writes; conversations, endpoints, and replica sets are derived from them
/// lazily at runtime. The flow names which side owns conversation identity (the
/// source); replication itself stays two-way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingRule {
    pub id: RuleId,
    pub flow: RoutingRuleFlow,
}

/// The shape of a rule: which chats on which connectors mirror each other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum RoutingRuleFlow {
    /// One fixed source chat mirrored into one fixed destination chat.
    OneToOne {
        source_connector: ConnectorId,
        source_chat: RuleChat,

        destination_connector: ConnectorId,
        destination_chat: RuleChat,
    },
    /// Every chat on the source connector that passes the filter, each mirrored
    /// into its own thread auto-created inside the destination container chat.
    /// The destination's `thread_id` is ignored: threads are provisioned per
    /// source conversation.
    ManyToOne {
        source_connector: ConnectorId,
        #[serde(default)]
        source_chats_filter: RuleChatsFilter,

        destination_connector: ConnectorId,
        destination_chat: RuleChat,
    },
}

/// A platform chat named by a rule (optionally one thread within it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleChat {
    pub chat_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

impl RuleChat {
    /// Whether this rule chat names the address: chat ids must match, and a
    /// thread-scoped rule chat only matches that thread while an unscoped one
    /// matches the whole chat.
    pub fn matches(&self, address: &PlatformAddress) -> bool {
        self.chat_id == address.chat_id
            && (self.thread_id.is_none() || self.thread_id == address.thread_id)
    }

    pub fn address(&self) -> PlatformAddress {
        PlatformAddress::new(self.chat_id.clone(), self.thread_id.clone())
    }
}

/// Which source chats a many-to-one rule covers. Struct variants (not tuples)
/// because serde cannot internally tag a newtype variant holding a sequence,
/// and the config lives in TOML.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum RuleChatsFilter {
    #[default]
    All,
    Only {
        chats: Vec<RuleChat>,
    },
    Not {
        chats: Vec<RuleChat>,
    },
}

impl RuleChatsFilter {
    pub fn accepts(&self, address: &PlatformAddress) -> bool {
        match self {
            RuleChatsFilter::All => true,
            RuleChatsFilter::Only { chats } => chats.iter().any(|chat| chat.matches(address)),
            RuleChatsFilter::Not { chats } => !chats.iter().any(|chat| chat.matches(address)),
        }
    }
}

/// One of the two ends of a rule's flow. The source end owns conversation
/// identity; replication itself is two-way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleEnd {
    Source,
    Destination,
}

impl RoutingRuleFlow {
    /// The connector serving the given end of this flow.
    pub fn connector(&self, end: RuleEnd) -> &ConnectorId {
        match (self, end) {
            (
                RoutingRuleFlow::OneToOne {
                    source_connector, ..
                },
                RuleEnd::Source,
            )
            | (
                RoutingRuleFlow::ManyToOne {
                    source_connector, ..
                },
                RuleEnd::Source,
            ) => source_connector,
            (
                RoutingRuleFlow::OneToOne {
                    destination_connector,
                    ..
                },
                RuleEnd::Destination,
            )
            | (
                RoutingRuleFlow::ManyToOne {
                    destination_connector,
                    ..
                },
                RuleEnd::Destination,
            ) => destination_connector,
        }
    }

    /// Whether the given end of this flow, served by `connector`, accepts an
    /// inbound address. A many-to-one destination matches on the container
    /// chat alone: its threads are minted per conversation, so a thread-level
    /// address still belongs to the container.
    pub fn end_accepts(
        &self,
        end: RuleEnd,
        connector: &ConnectorId,
        address: &PlatformAddress,
    ) -> bool {
        if self.connector(end) != connector {
            return false;
        }
        match (self, end) {
            (RoutingRuleFlow::OneToOne { source_chat, .. }, RuleEnd::Source) => {
                source_chat.matches(address)
            }
            (
                RoutingRuleFlow::OneToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => destination_chat.matches(address),
            (
                RoutingRuleFlow::ManyToOne {
                    source_chats_filter,
                    ..
                },
                RuleEnd::Source,
            ) => source_chats_filter.accepts(address),
            (
                RoutingRuleFlow::ManyToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => destination_chat.chat_id == address.chat_id,
        }
    }
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
    /// The last source title persisted for this conversation. Tracked so the
    /// core can detect when a source chat is renamed and fan a retitle out to
    /// the mirror threads. `None` until a title is first observed.
    #[serde(default)]
    pub title: Option<String>,
}

/// Records the mirror thread created for a conversation in a container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMapping {
    pub conversation_id: ConversationId,
    pub connector_id: ConnectorId,
    pub container: String,
    pub thread_id: String,
}
