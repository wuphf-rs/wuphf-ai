//! The observation log: a platform-agnostic record of every message a connector
//! reports, captured for read-only consumers (e.g. a TUI) regardless of whether
//! the message matched a routing rule. This is a separate concern from routing —
//! routing decides what to bridge, observation records what was seen — so the
//! two never share tables or resolution logic.

use crate::*;
use serde::{Deserialize, Serialize};

/// Whether an observed message arrived from the platform (`Incoming`) or was
/// sent out through the bridge on the owner's behalf (`Outgoing`, e.g. a TUI
/// reply). Lets a viewer align its own sends against inbound traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedDirection {
    Incoming,
    Outgoing,
}

/// One message captured in the observation log, keyed by its native coordinates
/// (`connector_id`, `chat_id`, `platform_message_id`). Holds enough to render a
/// timeline without touching the routing tables; attachments reference media by
/// id through the shared media store, and are metadata only (bytes are fetched
/// separately when a consumer needs them).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedMessage {
    pub connector_id: ConnectorId,
    pub platform: Platform,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub platform_message_id: String,
    pub sender: String,
    pub sender_name: Option<String>,
    pub participant: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to_platform_message_id: Option<String>,
    pub direction: ObservedDirection,
    pub timestamp_ms: i64,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub edited: bool,
}

/// Longest chat-list preview kept for a message, in characters. Bounds the chat
/// summary so a long message does not bloat the `observed_chats` row.
const PREVIEW_MAX_CHARS: usize = 80;

impl ObservedMessage {
    /// Capture an inbound create as an observation-log row: incoming, not yet
    /// edited or deleted. Borrows because the ingest path still owns the event
    /// afterwards.
    pub fn incoming(source: &SourceAddress, platform: Platform, inbound: &NewMessage) -> Self {
        Self {
            connector_id: source.connector_id.clone(),
            platform,
            chat_id: source.address.chat_id.clone(),
            thread_id: source.address.thread_id.clone(),
            platform_message_id: inbound.platform_message_id.clone(),
            sender: inbound.sender.clone(),
            sender_name: inbound.sender_name.clone(),
            participant: inbound.participant.clone(),
            text: inbound.text.clone(),
            attachments: inbound.attachments.clone(),
            reply_to_platform_message_id: inbound.reply_to_platform_message_id.clone(),
            direction: ObservedDirection::Incoming,
            timestamp_ms: inbound.timestamp_ms,
            deleted: false,
            edited: false,
        }
    }

    /// A short one-line summary of this message for a chat list: the trimmed text
    /// (truncated), or a bracketed label for the first attachment when there is
    /// no text, or empty when the message carries neither.
    pub fn preview(&self) -> String {
        if let Some(text) = self.text.as_deref() {
            let text = text.trim();
            if !text.is_empty() {
                return truncate(text, PREVIEW_MAX_CHARS);
            }
        }
        match self.attachments.first() {
            Some(attachment) => format!("[{}]", attachment_label(&attachment.kind)),
            None => String::new(),
        }
    }
}

/// A distinct chat seen by the observation log, summarised for a chat list. The
/// title is the freshest chat title a connector reported (kept if a later
/// message omits one); the preview and timestamp track the most recent message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedChat {
    pub connector_id: ConnectorId,
    pub platform: Platform,
    pub chat_id: String,
    pub title: Option<String>,
    pub last_message_ts: i64,
    pub last_message_preview: String,
}

fn attachment_label(kind: &AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Video => "video",
        AttachmentKind::Audio => "audio",
        AttachmentKind::File => "file",
        AttachmentKind::Sticker => "sticker",
    }
}

/// Truncate on a character boundary, appending an ellipsis when shortened, so a
/// preview never splits a multi-byte character.
fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
