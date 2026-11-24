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

/// The normalized, platform-agnostic message owned by a [`Conversation`].
/// This canonical form is copied out to every endpoint in the set as a
/// [`Dispatch`]. `author_id` is the platform id of whoever wrote the message;
/// `participant` is set when a mirror should label the message with its
/// author. `reply_to` references the parent by internal id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub author_id: String,
    pub participant: Option<String>,
    /// Human-readable display name of the author, resolved by the source
    /// connector. Used to label the author when mirroring into a shared chat;
    /// absent when the platform did not supply a name.
    #[serde(default)]
    pub author_name: Option<String>,
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
    /// native addressing (`native_id`) stays behind in the copy
    /// tables.
    pub fn from_inbound(
        inbound: NewMessage,
        conversation_id: ConversationId,
        reply_to: Option<MessageId>,
    ) -> Self {
        Self {
            id: MessageId::new(),
            conversation_id,
            author_id: inbound.author_id,
            participant: inbound.participant,
            author_name: inbound.author_name,
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
    pub native_id: String,
}

/// One reactor's reaction in an [`EventKind::ReactionSet`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactorReaction {
    pub reactor: String,
    pub reactor_name: Option<String>,
    pub emoji: String,
}

/// Who wrote a message's copy at one endpoint, in the terms a platform that
/// addresses a message by its author needs (a WhatsApp reaction key). When
/// the bridge wrote that copy the author is the bridge's own account;
/// otherwise `author_id`/`participant` are the target platform's own ids.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAuthor {
    pub author_id: String,
    pub participant: Option<String>,
    pub authored_by_bridge: bool,
}

impl MessageAuthor {
    /// Whether nothing is known of the author: a reaction routed before
    /// authors were carried, or one blocked until its copy is delivered.
    pub fn is_unknown(&self) -> bool {
        *self == Self::default()
    }

    /// The author of `message`'s copy at an endpoint, given whether the bridge
    /// sent that copy.
    pub fn of(message: &Message, authored_by_bridge: bool) -> Self {
        Self {
            author_id: message.author_id.clone(),
            participant: message.participant.clone(),
            authored_by_bridge,
        }
    }
}

/// The quoted parent of a reply, denormalized so a connector can render a quote
/// preview without a second lookup. Carries the parent's native id on the target
/// platform plus enough content to display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyContext {
    pub message_id: MessageId,
    pub native_id: String,
    pub author_id: String,
    pub participant: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    /// Whether the quoted copy at the target endpoint was written by the bridge
    /// rather than received from the target platform.
    ///
    /// It decides who a platform is told authored the quoted message. When the
    /// bridge sent that copy the author is the bridge's own account; when the
    /// copy was received there, the target *is* the message's origin, so
    /// `author_id`/`participant` are already that platform's own ids and name the
    /// real author. Platforms that address a quote by author (WhatsApp needs a
    /// participant alongside the quoted id) attribute it wrongly without this.
    #[serde(default)]
    pub authored_by_bridge: bool,
}

impl ReplyContext {
    /// Quote a stored parent message under the native id its copy carries at
    /// the target endpoint. `authored_by_bridge` reports whether that copy is
    /// one the bridge sent, which is what tells a connector whether to
    /// attribute the quote to its own account or to `participant`/`author_id`.
    pub fn quoting(parent: Message, native_id: String, authored_by_bridge: bool) -> Self {
        Self {
            message_id: parent.id,
            native_id,
            author_id: parent.author_id,
            participant: parent.participant,
            text: parent.text,
            attachments: parent.attachments,
            authored_by_bridge,
        }
    }
}

/// Identifies an inbound platform event so the same message seen twice (e.g. on
/// reconnect) is ingested only once.
///
/// Keyed by the connector that saw it, not the platform: two accounts in one
/// group (two WhatsApp connectors, say) each report every message under the
/// same native id, and each report belongs to that account's own conversation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    pub connector_id: ConnectorId,
    pub chat_id: String,
    pub native_id: String,
}

/// A new message as normalized by a connector, before the core resolves it to an
/// endpoint and group. Addresses are still native (`native_id`,
/// `reply_to_native_id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMessage {
    pub native_id: String,
    pub author_id: String,
    pub participant: Option<String>,
    /// Display name of the author as known to the source platform, if any.
    pub author_name: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to_native_id: Option<String>,
    pub timestamp_ms: i64,
    /// The chat's disappearing-messages timer as this message carried it, when
    /// it carried one. `None` never turns a known timer off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<DisappearingTimer>,
}

/// The kind of inbound event a connector reports for a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    NewMessage(NewMessage),
    Edit {
        native_id: String,
        text: Option<String>,
    },
    Delete {
        native_id: String,
    },
    /// A reactor set or cleared their reaction on a message (reactions are
    /// toggles). `native_id` is the reacted message, resolved to the logical
    /// message like an edit; `emoji` is `None` when the reactor cleared theirs;
    /// `from_me` flags the connector's own session account, which is the
    /// owner's on the platforms that report it.
    Reaction {
        native_id: String,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
        from_me: bool,
    },
    /// The complete set of other accounts' reactions on a message, for a
    /// platform that reports them as a whole (a Telegram user session). A
    /// reactor absent from `reactions` has none; the core diffs the set with
    /// what it stored for the copy.
    ReactionSet {
        native_id: String,
        reactions: Vec<ReactorReaction>,
    },
    /// A delivery or read receipt observed on the source platform. Surfaced on
    /// the message's other copies (a 👍 for delivered, 👀 for read in DMs).
    /// `native_id` names the receipted message; `observer` is the peer that
    /// delivered/read it; `from_me` flags the owner's own receipt, which the
    /// core ignores. The same path serves messages the bridge sent and
    /// messages the owner sent from another device, since the peer's receipt
    /// names the message regardless of which device authored it.
    Receipt {
        native_id: String,
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
pub struct Event {
    pub source: SourceAddress,
    pub payload: EventKind,
}
