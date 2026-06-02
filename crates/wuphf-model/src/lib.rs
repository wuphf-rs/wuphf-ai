//! Core domain types shared across the wuphf workspace.
//!
//! wuphf replicates messages between chat platforms. The model is built around
//! a few ideas, from configured intent down to a single delivery:
//!
//! - [`Rule`] — a user-declared mapping between two or more [`RuleSide`]s. Rules
//!   are the only routing input a user writes; everything below is derived.
//! - [`SideKind`] — how one side selects chats: a fixed `Single` chat, a
//!   `Source` set of real chats, or a threaded `Mirror` container.
//! - [`Endpoint`] — a concrete destination (a chat, optionally a thread) on one
//!   connector, identified internally by an [`EndpointId`]. Platform addresses
//!   resolve to it through an [`Alias`], never by equality, so an address can
//!   migrate without changing identity.
//! - [`Conversation`] — the abstract unit a rule pairs across sides (one per
//!   source chat). Backed by one [`ReplicaSet`].
//! - [`ReplicaSet`] — the set of endpoints that mirror each other; a
//!   [`Message`] delivered to one is dispatched to all the others.
//! - [`Message`] — the normalized, platform-agnostic form copied out to every
//!   endpoint as a [`StoredDispatch`].
//!
//! Connectors are thin pipes: they report [`ConnectorInput`]s keyed by raw
//! platform addresses and execute [`ConnectorOutput`] dispatches. All routing
//! and identity resolution lives in the core.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

uuid_id!(
    MessageId,
    "Stable internal id of a normalized [`Message`]. Native platform message ids map to it via [`MessageCopy`]."
);
uuid_id!(
    ReplicaSetId,
    "Id of a [`ReplicaSet`] — the set of endpoints a message fans out to."
);
uuid_id!(
    EndpointId,
    "Stable internal id of an [`Endpoint`]. Platform addresses resolve to it through [`Alias`]es, never by equality."
);
uuid_id!(
    DispatchId,
    "Id of one outbound [`StoredDispatch`] targeting a single endpoint."
);
uuid_id!(
    MediaId,
    "Id of a stored media blob referenced by an [`Attachment`]."
);
uuid_id!(
    ConversationId,
    "Id of a [`Conversation`] — the unit a [`Rule`] pairs across its sides."
);

/// User-chosen id of a routing [`Rule`], taken verbatim from configuration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

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

/// Capabilities a connector advertises at registration. A `Mirror` side is only
/// legal on a connector that supports both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorCapabilities {
    pub supports_threads: bool,
    pub supports_topic_create: bool,
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

/// Media category of an [`Attachment`], used to pick the platform send method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    File,
    Sticker,
}

/// Metadata for one piece of media. The bytes are stored separately and fetched
/// by `media_id` through the media broker (see [`MediaBlob`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub media_id: MediaId,
    pub kind: AttachmentKind,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<u64>,
}

/// An [`Attachment`] together with its raw bytes, as moved through the media
/// broker. Not serialized — the bytes never travel in a message payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaBlob {
    pub attachment: Attachment,
    pub bytes: Vec<u8>,
}

/// The normalized, platform-agnostic message owned by a [`ReplicaSet`].
/// This canonical form is copied out to every endpoint in the set as a
/// [`StoredDispatch`]. `sender` is the chat; `participant` is the individual
/// author in a group chat. `reply_to` references the parent by internal id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub replica_set_id: ReplicaSetId,
    pub sender: String,
    pub participant: Option<String>,
    /// Human-readable display name of the author, resolved by the source
    /// connector. Used to label the author when mirroring into a shared chat;
    /// absent when the platform did not supply a name.
    #[serde(default)]
    pub sender_name: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<MessageId>,
    pub timestamp_ms: i64,
    /// Set once the source revoked the message. Retained (not removed) so its
    /// native ids still resolve, letting a re-reported delete be recognized as
    /// redundant and breaking the delete echo loop with mirror platforms.
    #[serde(default)]
    pub deleted: bool,
}

/// Records the native id under which a [`Message`] was delivered to one
/// endpoint. Maps internal message identity to per-platform message ids, which
/// powers reply resolution and dedup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCopy {
    pub message_id: MessageId,
    pub endpoint_id: EndpointId,
    pub platform_message_id: String,
}

/// The quoted parent of a reply, denormalized so a connector can render a quote
/// preview without a second lookup. Carries the parent's native id on the target
/// platform plus enough content to display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyContext {
    pub message_id: MessageId,
    pub platform_message_id: String,
    pub sender: String,
    pub participant: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// Identifies an inbound platform event so the same message seen twice (e.g. on
/// reconnect) is ingested only once.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    pub platform: Platform,
    pub chat_id: String,
    pub platform_message_id: String,
}

/// Cached display-name fields for a sender, used to label mirrored messages and
/// newly provisioned threads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub first_name: Option<String>,
    pub full_name: Option<String>,
    pub push_name: Option<String>,
    pub business_name: Option<String>,
}

/// Per-endpoint chat options, e.g. a disappearing-message timer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSettings {
    pub endpoint_id: EndpointId,
    pub is_ephemeral: bool,
    pub ephemeral_timer_seconds: Option<u64>,
}

/// A new message as normalized by a connector, before the core resolves it to an
/// endpoint and group. Addresses are still native (`platform_message_id`,
/// `reply_to_platform_message_id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessage {
    pub platform_message_id: String,
    pub sender: String,
    pub participant: Option<String>,
    /// Display name of the author as known to the source platform, if any.
    pub sender_name: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to_platform_message_id: Option<String>,
    pub timestamp_ms: i64,
}

/// An emoji reaction added to a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub message_id: MessageId,
    pub user_id: String,
    pub emoji: String,
}

/// The kind of inbound event a connector reports for a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventPayload {
    MessageCreate(InboundMessage),
    MessageEdit {
        platform_message_id: String,
        text: Option<String>,
    },
    MessageDelete {
        platform_message_id: String,
    },
    ReactionAdd(Reaction),
}

/// Raw origin coordinates for an inbound event. The connector supplies its id
/// and the platform chat address; the core resolves these to an endpoint,
/// lazily minting endpoint/conversation/group when a rule matches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAddress {
    pub connector_id: ConnectorId,
    pub address: PlatformAddress,
    /// Optional display name used to title a newly provisioned mirror thread.
    #[serde(default)]
    pub chat_title: Option<String>,
}

/// A connector event paired with its origin coordinates, ready for the core to
/// resolve against the alias table and rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub source: SourceAddress,
    pub payload: EventPayload,
}

/// The content of an outbound dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchPayload {
    MessageCreate {
        message: Message,
        reply_context: Option<ReplyContext>,
    },
    /// Revise an already-delivered copy. `target_platform_message_id` is the
    /// native id of the copy to edit at the target endpoint; it is empty while
    /// the dispatch is blocked and filled in when the original create is
    /// acknowledged.
    MessageEdit {
        message: Message,
        target_platform_message_id: String,
    },
    /// Revoke an already-delivered copy. `target_platform_message_id` is the
    /// native id of the copy to delete at the target endpoint; it is empty while
    /// the dispatch is blocked and filled in when the original create is
    /// acknowledged.
    MessageDelete {
        message: Message,
        target_platform_message_id: String,
    },
}

/// Instruction to create a thread/topic in a container before sending, used
/// when a mirror conversation has no thread yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionThread {
    pub container: String,
    pub title: String,
}

/// The resolved platform destination for a dispatch. The connector sends to
/// `address`; if `provision_thread` is set it first creates the thread and
/// reports the resulting thread id back in the acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetAddress {
    pub address: PlatformAddress,
    #[serde(default)]
    pub provision_thread: Option<ProvisionThread>,
}

/// A fully resolved instruction to deliver one payload to one endpoint on one
/// connector. The core produces a command per group member when fanning out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchCommand {
    pub target_endpoint: EndpointId,
    pub target_connector: ConnectorId,
    pub target: TargetAddress,
    pub payload: DispatchPayload,
}

impl DispatchCommand {
    pub fn message_id(&self) -> MessageId {
        match &self.payload {
            DispatchPayload::MessageCreate { message, .. }
            | DispatchPayload::MessageEdit { message, .. }
            | DispatchPayload::MessageDelete { message, .. } => message.id,
        }
    }

    pub fn message_replica_set_id(&self) -> ReplicaSetId {
        match &self.payload {
            DispatchPayload::MessageCreate { message, .. }
            | DispatchPayload::MessageEdit { message, .. }
            | DispatchPayload::MessageDelete { message, .. } => message.replica_set_id,
        }
    }
}

/// Lifecycle of a [`StoredDispatch`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchState {
    /// Ready to send; redelivered to the connector until acknowledged.
    Pending,
    /// Held until the message it replies to is itself delivered, so the reply can
    /// quote the parent's native id.
    Blocked { waiting_for: MessageId },
    /// Delivered; carries the native id the connector assigned.
    Acknowledged { platform_message_id: String },
    /// The connector reported the dispatch can never succeed. It is retained for
    /// observability but is no longer pending, so it is never redelivered.
    Failed { reason: String },
}

/// A [`DispatchCommand`] together with its persisted [`DispatchState`] — one
/// outbound delivery the core tracks to completion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDispatch {
    pub id: DispatchId,
    pub command: DispatchCommand,
    pub state: DispatchState,
}

/// A connector's confirmation that a dispatch was delivered, returned after a
/// successful send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchAcknowledgement {
    pub dispatch_id: DispatchId,
    pub platform_message_id: String,
    /// The thread/topic the message landed in. Set when the dispatch provisioned
    /// a new thread, so the core can persist the conversation thread mapping.
    #[serde(default)]
    pub thread_id: Option<String>,
}

/// Stable id of a connector, matching a `[[connector]]` entry in core config and
/// the registration a connector sends on connect.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectorId(pub String);

impl fmt::Display for ConnectorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A connector declared in core configuration. Endpoints are no longer
/// pre-assigned; ownership is derived from the rules that reference the
/// connector, and the connector advertises its capabilities at registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorRegistration {
    pub id: ConnectorId,
    pub platform: Platform,
    #[serde(default)]
    pub capabilities: ConnectorCapabilities,
}

/// A frame a connector sends up to the core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorInput {
    Event(NormalizedEvent),
    Acknowledgement(DispatchAcknowledgement),
    /// The platform reported that a chat address changed (e.g. WhatsApp
    /// phone-to-LID). Both addresses remain resolvable; the new one becomes
    /// canonical for outbound sends.
    IdentityMigrated {
        connector_id: ConnectorId,
        old: PlatformAddress,
        new: PlatformAddress,
    },
    /// The connector reported a dispatch can never succeed, so the core should
    /// dead-letter it instead of redelivering on the next session.
    DispatchFailed {
        dispatch_id: DispatchId,
        reason: String,
    },
}

/// A frame the core sends down to a connector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorOutput {
    /// A dispatch to execute, then acknowledge.
    Dispatch(Box<StoredDispatch>),
    /// The core refused the connector's last input (e.g. unknown registration).
    Rejected { reason: String },
}
