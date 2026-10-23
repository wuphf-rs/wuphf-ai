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
    pub fn address(&self) -> ChatAddress {
        ChatAddress {
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
pub struct ChatAddress {
    pub chat_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

impl ChatAddress {
    pub fn new(chat_id: impl Into<String>, thread_id: Option<String>) -> Self {
        Self {
            chat_id: chat_id.into(),
            thread_id,
        }
    }

    /// Whether this address, used as a rule's chat selector, names the chat
    /// known by any of `addresses`. Chat ids must match; a selector with no
    /// thread names the whole chat, one with a thread names only that thread.
    /// Any form in the set counts, so a rule written with one of a chat's
    /// addresses still names it when the platform reports another.
    pub fn selects(&self, addresses: &AddressSet) -> bool {
        addresses.iter().any(|address| {
            self.chat_id == address.chat_id
                && (self.thread_id.is_none() || self.thread_id == address.thread_id)
        })
    }
}

/// Every platform address known to name one chat: the address an event actually
/// arrived on (the primary) plus the equivalent forms the platform has reported
/// for it, as recorded by [`AddressLink`].
///
/// Platforms hand out several interchangeable addresses for the same peer —
/// WhatsApp reaches a contact as both a phone JID and a LID — and a user writing
/// a rule picks whichever form they know. Matching a rule against the whole set
/// rather than one string is what makes a chat list agree with the platform no
/// matter which form the user wrote or the connector reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressSet {
    primary: ChatAddress,
    alternates: Vec<ChatAddress>,
}

impl AddressSet {
    /// A set with no known alternates. Every connector that hands out one stable
    /// address per chat resolves through this.
    pub fn single(primary: ChatAddress) -> Self {
        Self {
            primary,
            alternates: Vec::new(),
        }
    }

    /// A set of known-equivalent forms. `alternates` may repeat the primary or
    /// each other; duplicates are dropped so callers can pass a raw closure.
    pub fn new(primary: ChatAddress, alternates: Vec<ChatAddress>) -> Self {
        let mut set = Self::single(primary);
        for address in alternates {
            set.insert(address);
        }
        set
    }

    /// Add an equivalent form, ignoring one already in the set.
    pub fn insert(&mut self, address: ChatAddress) {
        if address != self.primary && !self.alternates.contains(&address) {
            self.alternates.push(address);
        }
    }

    /// The address the event arrived on. This is the form the core stores and
    /// routes with; the alternates only widen matching.
    pub fn primary(&self) -> &ChatAddress {
        &self.primary
    }

    /// Every form in the set, primary first.
    pub fn iter(&self) -> impl Iterator<Item = &ChatAddress> {
        std::iter::once(&self.primary).chain(self.alternates.iter())
    }
}

impl From<ChatAddress> for AddressSet {
    fn from(address: ChatAddress) -> Self {
        Self::single(address)
    }
}

/// A platform-reported equivalence between two addresses on one connector (e.g.
/// a WhatsApp phone JID and the LID for the same contact). Recorded whether or
/// not either address has an [`Endpoint`] yet, which is what lets rule matching
/// treat the forms as one chat from the very first message — an [`Alias`], by
/// contrast, only exists once an endpoint does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressLink {
    pub connector_id: ConnectorId,
    pub address: ChatAddress,
    pub linked_address: ChatAddress,
}

/// A user-declared replication mapping. Rules are the only routing input a user
/// writes; conversations, endpoints, and replica sets are derived from them
/// lazily at runtime. The flow names which side owns conversation identity (the
/// source); replication itself stays two-way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub flow: RuleKind,
}

/// The shape of a rule: which chats on which connectors mirror each other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum RuleKind {
    /// One fixed source chat mirrored into one fixed destination chat.
    OneToOne {
        source_connector: ConnectorId,
        source_chat: ChatAddress,

        destination_connector: ConnectorId,
        destination_chat: ChatAddress,
    },
    /// Every chat on the source connector that passes the filter, each mirrored
    /// into its own thread auto-created inside the destination container chat.
    /// The destination's `thread_id` is ignored: threads are provisioned per
    /// source conversation.
    ManyToOne {
        source_connector: ConnectorId,
        #[serde(default)]
        source_chats_filter: ChatFilter,

        destination_connector: ConnectorId,
        destination_chat: ChatAddress,
    },
}

/// Which source chats a many-to-one rule covers. Struct variants (not tuples)
/// because serde cannot internally tag a newtype variant holding a sequence,
/// and the config lives in TOML.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum ChatFilter {
    #[default]
    All,
    Only {
        chats: Vec<ChatAddress>,
    },
    Not {
        chats: Vec<ChatAddress>,
    },
}

impl ChatFilter {
    /// Whether a chat, named by every address form known for it, passes. Both
    /// list kinds test the whole set: an allow list admits a chat listed under
    /// any of its forms, and a deny list excludes one listed under any of its
    /// forms — so a chat cannot slip past a deny list by arriving under the
    /// address the user did not write.
    pub fn accepts(&self, addresses: &AddressSet) -> bool {
        match self {
            ChatFilter::All => true,
            ChatFilter::Only { chats } => chats.iter().any(|chat| chat.selects(addresses)),
            ChatFilter::Not { chats } => !chats.iter().any(|chat| chat.selects(addresses)),
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

impl RuleKind {
    /// The connector serving the given end of this flow.
    pub fn connector(&self, end: RuleEnd) -> &ConnectorId {
        match (self, end) {
            (
                RuleKind::OneToOne {
                    source_connector, ..
                },
                RuleEnd::Source,
            )
            | (
                RuleKind::ManyToOne {
                    source_connector, ..
                },
                RuleEnd::Source,
            ) => source_connector,
            (
                RuleKind::OneToOne {
                    destination_connector,
                    ..
                },
                RuleEnd::Destination,
            )
            | (
                RuleKind::ManyToOne {
                    destination_connector,
                    ..
                },
                RuleEnd::Destination,
            ) => destination_connector,
        }
    }

    /// Whether the given end of this flow, served by `connector`, accepts an
    /// inbound chat, named by every address form known for it. A many-to-one
    /// destination matches on the container chat alone: its threads are minted
    /// per conversation, so a thread-level address still belongs to the
    /// container.
    pub fn end_accepts(
        &self,
        end: RuleEnd,
        connector: &ConnectorId,
        addresses: &AddressSet,
    ) -> bool {
        if self.connector(end) != connector {
            return false;
        }
        match (self, end) {
            (RuleKind::OneToOne { source_chat, .. }, RuleEnd::Source) => {
                source_chat.selects(addresses)
            }
            (
                RuleKind::OneToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => destination_chat.selects(addresses),
            (
                RuleKind::ManyToOne {
                    source_chats_filter,
                    ..
                },
                RuleEnd::Source,
            ) => source_chats_filter.accepts(addresses),
            (
                RuleKind::ManyToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => addresses
                .iter()
                .any(|address| destination_chat.chat_id == address.chat_id),
        }
    }
}

/// A resolvable platform address for an endpoint. Many aliases may point at one
/// endpoint (e.g. after a WhatsApp phone-to-LID migration); exactly one is
/// canonical for outbound sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    pub connector_id: ConnectorId,
    pub address: ChatAddress,
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
    pub source_address: ChatAddress,
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
