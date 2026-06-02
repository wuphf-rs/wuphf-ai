//! The normalized message and its parts: attachments, media, reply context,
//! and the inbound events a connector reports before the core resolves them.

use crate::*;
use serde::{Deserialize, Serialize};

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
