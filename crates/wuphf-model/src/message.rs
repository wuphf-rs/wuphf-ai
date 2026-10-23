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
    /// Whether the media plays: an animated sticker or a GIF (a `Video` with GIF
    /// playback semantics). A target uses it to render motion natively, as the
    /// mime type alone cannot tell a static WebP from an animated one, or a GIF
    /// from a normal video.
    #[serde(default)]
    pub animated: bool,
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
/// [`Dispatch`]. `sender` is the chat; `participant` is the individual
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

impl Message {
    /// Promote a resolved inbound message to the canonical logical form,
    /// minting its internal id. `reply_to` is the already-resolved parent;
    /// native addressing (`platform_message_id`) stays behind in the copy
    /// tables.
    pub fn from_inbound(
        inbound: InboundMessage,
        replica_set_id: ReplicaSetId,
        reply_to: Option<MessageId>,
    ) -> Self {
        Self {
            id: MessageId::new(),
            replica_set_id,
            sender: inbound.sender,
            participant: inbound.participant,
            sender_name: inbound.sender_name,
            text: inbound.text,
            attachments: inbound.attachments,
            reply_to,
            timestamp_ms: inbound.timestamp_ms,
            deleted: false,
        }
    }
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
    /// Whether the quoted copy at the target endpoint was written by the bridge
    /// rather than received from the target platform.
    ///
    /// It decides who a platform is told authored the quoted message. When the
    /// bridge sent that copy the author is the bridge's own account; when the
    /// copy was received there, the target *is* the message's origin, so
    /// `sender`/`participant` are already that platform's own ids and name the
    /// real author. Platforms that address a quote by author (WhatsApp needs a
    /// participant alongside the quoted id) attribute it wrongly without this.
    #[serde(default)]
    pub authored_by_bridge: bool,
}

impl ReplyContext {
    /// Quote a stored parent message under the native id its copy carries at
    /// the target endpoint. `authored_by_bridge` reports whether that copy is
    /// one the bridge sent, which is what tells a connector whether to
    /// attribute the quote to its own account or to `participant`/`sender`.
    pub fn quoting(parent: Message, platform_message_id: String, authored_by_bridge: bool) -> Self {
        Self {
            message_id: parent.id,
            platform_message_id,
            sender: parent.sender,
            participant: parent.participant,
            text: parent.text,
            attachments: parent.attachments,
            authored_by_bridge,
        }
    }
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
    /// A reactor set or cleared their reaction on a message (reactions are
    /// toggles). `platform_message_id` is the reacted message's native id,
    /// resolved to the logical message like an edit; `emoji` is `None` when the
    /// reactor cleared theirs; `from_me` flags the account owner so the core can
    /// recognise a reaction it applied being echoed back.
    ReactionSet {
        platform_message_id: String,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
        from_me: bool,
    },
    /// A delivery or read receipt observed on the source platform. Surfaced on
    /// the message's other copies (a 👍 for delivered, 👀 for read in DMs).
    /// `platform_message_id` is the receipted message's native id; `observer` is
    /// the peer that delivered/read it; `from_me` flags the owner's own receipt,
    /// which the core ignores. The same path serves messages the bridge sent and
    /// messages the owner sent from another device, since the peer's receipt
    /// names the message regardless of which device authored it.
    ReceiptSet {
        platform_message_id: String,
        observer: String,
        from_me: bool,
        kind: ReceiptKind,
    },
}

/// Whether a receipt reports the message reached the peer's device (delivered)
/// or was opened by the peer (read). Picks the signal the connector renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    Delivered,
    Read,
}

/// Raw origin coordinates for an inbound event. The connector supplies its id
/// and the platform chat address; the core resolves these to an endpoint,
/// lazily minting endpoint/conversation/group when a rule matches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAddress {
    pub connector_id: ConnectorId,
    pub address: ChatAddress,
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
