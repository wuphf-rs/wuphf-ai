use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
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

uuid_id!(MessageId);
uuid_id!(GroupId);
uuid_id!(EndpointId);
uuid_id!(DispatchId);
uuid_id!(MediaId);
uuid_id!(ConversationId);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicationGroup {
    pub id: GroupId,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSide {
    pub connector: ConnectorId,
    #[serde(flatten)]
    pub kind: SideKind,
}

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

/// The abstract unit a rule pairs across sides, backed by one replication group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub rule_id: RuleId,
    pub group_id: GroupId,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    File,
    Sticker,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub media_id: MediaId,
    pub kind: AttachmentKind,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaBlob {
    pub attachment: Attachment,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub group_id: GroupId,
    pub sender: String,
    pub participant: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<MessageId>,
    pub timestamp_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCopy {
    pub message_id: MessageId,
    pub endpoint_id: EndpointId,
    pub platform_message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyContext {
    pub message_id: MessageId,
    pub platform_message_id: String,
    pub sender: String,
    pub participant: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    pub platform: Platform,
    pub chat_id: String,
    pub platform_message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub first_name: Option<String>,
    pub full_name: Option<String>,
    pub push_name: Option<String>,
    pub business_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSettings {
    pub endpoint_id: EndpointId,
    pub is_ephemeral: bool,
    pub ephemeral_timer_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessage {
    pub platform_message_id: String,
    pub sender: String,
    pub participant: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to_platform_message_id: Option<String>,
    pub timestamp_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub message_id: MessageId,
    pub user_id: String,
    pub emoji: String,
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub source: SourceAddress,
    pub payload: EventPayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchPayload {
    MessageCreate {
        message: Message,
        reply_context: Option<ReplyContext>,
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
            DispatchPayload::MessageCreate { message, .. } => message.id,
        }
    }

    pub fn message_group_id(&self) -> GroupId {
        match &self.payload {
            DispatchPayload::MessageCreate { message, .. } => message.group_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchState {
    Pending,
    Blocked { waiting_for: MessageId },
    Acknowledged { platform_message_id: String },
    /// The connector reported the dispatch can never succeed. It is retained for
    /// observability but is no longer pending, so it is never redelivered.
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDispatch {
    pub id: DispatchId,
    pub command: DispatchCommand,
    pub state: DispatchState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchAcknowledgement {
    pub dispatch_id: DispatchId,
    pub platform_message_id: String,
    /// The thread/topic the message landed in. Set when the dispatch provisioned
    /// a new thread, so the core can persist the conversation thread mapping.
    #[serde(default)]
    pub thread_id: Option<String>,
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorOutput {
    Dispatch(Box<StoredDispatch>),
    Rejected { reason: String },
}
