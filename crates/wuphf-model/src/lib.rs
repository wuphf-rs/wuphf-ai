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
    pub platform: Platform,
    pub chat_id: String,
    pub thread_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicationGroup {
    pub id: GroupId,
    pub endpoints: Vec<EndpointId>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub origin_endpoint: EndpointId,
    pub group_id: GroupId,
    pub payload: EventPayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchPayload {
    MessageCreate {
        message: Message,
        reply_context: Option<ReplyContext>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchCommand {
    pub target_endpoint: EndpointId,
    pub payload: DispatchPayload,
}

impl DispatchCommand {
    pub fn message_id(&self) -> MessageId {
        match &self.payload {
            DispatchPayload::MessageCreate { message, .. } => message.id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchState {
    Pending,
    Blocked { waiting_for: MessageId },
    Acknowledged { platform_message_id: String },
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
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectorId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorRegistration {
    pub id: ConnectorId,
    pub platform: Platform,
    pub endpoints: Vec<EndpointId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorInput {
    Event(NormalizedEvent),
    Acknowledgement(DispatchAcknowledgement),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorOutput {
    Dispatch(Box<StoredDispatch>),
    Rejected { reason: String },
}
