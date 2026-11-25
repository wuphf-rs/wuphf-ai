//! Outbound delivery: one [`Dispatch`] per instruction to one connector, where
//! it goes ([`Target`]) and what it does ([`Action`]).

use crate::*;
use serde::{Deserialize, Serialize};

/// One instruction to one connector, tracked until it is delivered or fails.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    pub id: DispatchId,
    pub state: DispatchState,
    pub to: Target,
    /// The conversation the dispatch belongs to. Keys thread provisioning.
    pub conversation_id: ConversationId,
    pub action: Action,
}

impl Dispatch {
    /// A new dispatch: `Pending`, or `Blocked` until `waiting_for` has been
    /// delivered to the same endpoint.
    pub fn new(
        to: Target,
        conversation_id: ConversationId,
        action: Action,
        waiting_for: Option<MessageId>,
    ) -> Self {
        Self {
            id: DispatchId::new(),
            state: match waiting_for {
                Some(waiting_for) => DispatchState::Blocked { waiting_for },
                None => DispatchState::Pending,
            },
            to,
            conversation_id,
            action,
        }
    }

    /// The logical message the dispatch acts on, or `None` for an action on a
    /// conversation rather than a message (a thread rename, a note). The id keys the
    /// delivered copy a later edit/delete revises.
    pub fn message_id(&self) -> Option<MessageId> {
        match &self.action {
            Action::Send { message, .. } | Action::Edit { message, .. } => Some(message.id),
            Action::Delete { message_id, .. }
            | Action::React { message_id, .. }
            | Action::Receipt { message_id, .. }
            | Action::Refuse { message_id, .. } => Some(*message_id),
            Action::RenameThread { .. } | Action::Note { .. } | Action::SetChatState { .. } => None,
        }
    }

    /// Whether acknowledging this dispatch records a delivered copy. Only sends,
    /// edits and deletes establish (or re-affirm) a copy; reactions, receipts
    /// and renames act on existing copies and must not record one — a signal
    /// targeting the origin copy would otherwise flip its received copy to a
    /// sent copy.
    pub fn records_copy(&self) -> bool {
        matches!(
            self.action,
            Action::Send { .. } | Action::Edit { .. } | Action::Delete { .. }
        )
    }

    pub fn is_pending(&self) -> bool {
        self.state == DispatchState::Pending
    }
}

/// Where a dispatch goes: one endpoint, reached through its connector at a
/// platform chat address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub endpoint_id: EndpointId,
    pub connector_id: ConnectorId,
    pub address: ChatAddress,
    /// When set, the connector first creates a thread with this title inside
    /// `address.chat_id`, sends into it, and reports the new thread id in its
    /// acknowledgement. Used when a mirror conversation has no thread yet.
    #[serde(default)]
    pub create_thread: Option<String>,
}

/// What a dispatch does at its target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Deliver a new copy. How the copy is rendered (attachments-as-files,
    /// text spillover, and the like) is the target connector's own
    /// configuration, so the dispatch carries only content.
    Send {
        message: Message,
        reply: Option<ReplyContext>,
        /// The target chat's disappearing-messages timer when the send was
        /// routed: `None` when unknown or when the target connector does not
        /// stamp timers. Content sends carry it; edits, deletes and reactions
        /// do not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disappearing: Option<DisappearingTimer>,
    },
    /// Revise an already-delivered copy. `native_id` is the native id of the
    /// copy to edit at the target endpoint; it is empty while the dispatch is
    /// blocked and filled in when the original send is acknowledged.
    Edit { message: Message, native_id: String },
    /// Revoke an already-delivered copy. `native_id` is the native id of the
    /// copy to delete at the target endpoint; it is empty while the dispatch is
    /// blocked and filled in when the original send is acknowledged.
    Delete {
        message_id: MessageId,
        native_id: String,
    },
    /// Reflect a reaction change on an already-delivered copy. The core only
    /// routes the signal; the connector decides how to render it (e.g. Telegram
    /// shows text rather than a native reaction). `native_id` is the native id
    /// of the reacted copy at the target endpoint — empty while the dispatch is
    /// blocked, filled when the original send is acknowledged.
    ///
    /// `by_owner` says the reactor is the owner, which a platform reacting as
    /// the owner's account applies natively. `target_author` is who wrote the
    /// reacted copy at the target, filled with `native_id`.
    React {
        message_id: MessageId,
        native_id: String,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        by_owner: bool,
        #[serde(default, skip_serializing_if = "MessageAuthor::is_unknown")]
        target_author: MessageAuthor,
        /// The owner's lone-emoji reply this reaction came from (plan 081):
        /// marked as delivered once the reaction is acknowledged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirm: Option<MessageCopyRef>,
    },
    /// Signal the origin copy that its message reached all of its mirrors
    /// (`Delivered`) or was read on one (`Read`). The connector decides how to
    /// surface it (e.g. a 👍 reaction, or 👀 in DMs).
    Receipt {
        message_id: MessageId,
        native_id: String,
        kind: ReceiptKind,
    },
    /// Tell the origin copy that the bridge did not send it on (plan 083): a
    /// message into a receive-only chat, a story quoted outside its poster's
    /// chat. The connector marks the copy and posts `reason` as a reply note
    /// it deletes after `note_ttl_secs`. Records no copy.
    Refuse {
        message_id: MessageId,
        native_id: String,
        reason: String,
        note_ttl_secs: u32,
    },
    /// Rename an already-provisioned mirror thread when its source
    /// conversation's title changes. Carries no message because a rename is a
    /// conversation property, not a per-message operation; the thread is
    /// addressed by the dispatch's [`Target`].
    RenameThread { title: String },
    /// A system note the core composed about the chat: an info card, or a
    /// change to the chat's profile. It is no logical message: acknowledging
    /// it records no copy, so nothing can reply to it or bridge it back.
    Note {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
    },
    /// Bring the target chat (or thread) to this pin / mute / archive state;
    /// absent fields are left alone. It acts on no message and records no
    /// copy.
    SetChatState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pinned: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mute: Option<MuteState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archived: Option<bool>,
    },
}

/// Lifecycle of a [`Dispatch`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchState {
    /// Ready to send; redelivered to the connector until acknowledged.
    Pending,
    /// Held until the message it replies to is itself delivered, so the reply can
    /// quote the parent's native id.
    Blocked { waiting_for: MessageId },
    /// Delivered; carries the native id the connector assigned.
    Acknowledged { native_id: String },
    /// The connector reported the dispatch can never succeed. It is retained for
    /// observability but is no longer pending, so it is never redelivered.
    Failed { reason: String },
}

/// A connector's confirmation that a dispatch was delivered, returned after a
/// successful send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchAck {
    pub dispatch_id: DispatchId,
    pub native_id: String,
    /// The thread/topic the message landed in. Set when the dispatch provisioned
    /// a new thread, so the core can stamp the thread onto the mirror endpoint.
    #[serde(default)]
    pub thread_id: Option<String>,
}

/// The core's verdict on one ingested connector event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    Routed {
        message_id: MessageId,
        dispatches: Vec<Dispatch>,
        /// How many dispatches were parked until another message is delivered
        /// first. They are not in `dispatches`; they are released when their
        /// parent is acknowledged.
        blocked: usize,
    },
    /// Held because the conversation's mirror thread is still being
    /// provisioned; the message is replayed once the thread exists.
    Deferred {
        message_id: MessageId,
    },
    Ignored(IgnoreReason),
}

impl IngestOutcome {
    pub fn dispatches(&self) -> &[Dispatch] {
        match self {
            Self::Routed { dispatches, .. } => dispatches,
            Self::Deferred { .. } | Self::Ignored(_) => &[],
        }
    }
}

/// Why an ingested event produced no dispatches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgnoreReason {
    DuplicateEvent,
    SentMessageEcho,
    Unmatched,
    /// An edit that leaves the stored text unchanged: a redundant report or our
    /// own mirrored edit echoed back by the platform.
    RedundantEdit,
    /// A delete of an already-revoked message: a redundant report or our own
    /// mirrored delete echoed back by the platform.
    RedundantDelete,
    /// A reaction equal to the one stored for that reactor on that copy: a
    /// replay, or the platform echoing a reaction the bridge applied.
    RedundantReaction,
}

/// The `author_id` stamped on owner messages. Platforms deliver the message
/// from the owner's own account, so this label only surfaces in the bridge's
/// own records and mirror renderings.
pub const OWNER_AUTHOR: &str = "me";

/// An owner-authored outbound message addressed by raw platform coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerMessage {
    pub connector_id: ConnectorId,
    pub chat_id: String,
    pub thread_id: Option<String>,
    /// May be empty when `media_ids` is not.
    pub text: String,
    /// Attachments, as ids of media already stored in the core; the core sends
    /// each with its stored metadata.
    pub media_ids: Vec<MediaId>,
    /// The client that sent it (e.g. `wuphf-tui`), kept on its outgoing
    /// observed row as [`crate::ObservedMessage::sent_by`].
    pub client: Option<String>,
    /// Native id of the message this send replies to, when known. Resolved
    /// best-effort: a reply to a message the bridge never stored sends as a
    /// plain message rather than failing.
    pub reply_to_native_id: Option<String>,
    /// Send time in epoch milliseconds, supplied by the caller so the core
    /// stays clock-free.
    pub timestamp_ms: i64,
}

/// The dispatches an owner message produced: the direct delivery to the addressed
/// chat, plus any mirror copies routing fanned out (empty for unrouted chats
/// and for sends deferred behind mirror-thread provisioning).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerSendOutcome {
    pub direct: Dispatch,
    pub mirrored: Vec<Dispatch>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            endpoint_id: EndpointId::new(),
            connector_id: ConnectorId("telegram".into()),
            address: ChatAddress::new("-100", None),
            create_thread: None,
        }
    }

    fn message() -> Message {
        Message {
            id: MessageId::new(),
            conversation_id: ConversationId::new(),
            author_id: "alice".into(),
            participant: None,
            author_name: None,
            text: Some("hi".into()),
            attachments: Vec::new(),
            reply_to: None,
            timestamp_ms: 0,
            deleted: false,
        }
    }

    /// One action of every kind, paired with whether it records a copy.
    fn actions() -> Vec<(Action, bool)> {
        let message = message();
        vec![
            (
                Action::Send {
                    message: message.clone(),
                    reply: None,
                    disappearing: None,
                },
                true,
            ),
            (
                Action::Edit {
                    message: message.clone(),
                    native_id: String::new(),
                },
                true,
            ),
            (
                Action::Delete {
                    message_id: message.id,
                    native_id: String::new(),
                },
                true,
            ),
            (
                Action::React {
                    message_id: message.id,
                    native_id: String::new(),
                    reactor: "bob".into(),
                    reactor_name: None,
                    emoji: None,
                    by_owner: false,
                    target_author: MessageAuthor::default(),
                    confirm: None,
                },
                false,
            ),
            (
                Action::Receipt {
                    message_id: message.id,
                    native_id: String::new(),
                    kind: ReceiptKind::Delivered,
                },
                false,
            ),
            (
                Action::Refuse {
                    message_id: message.id,
                    native_id: "n".into(),
                    reason: "no".into(),
                    note_ttl_secs: 30,
                },
                false,
            ),
            (
                Action::RenameThread {
                    title: "title".into(),
                },
                false,
            ),
            (
                Action::Note {
                    text: "note".into(),
                    attachments: Vec::new(),
                },
                false,
            ),
            (
                Action::SetChatState {
                    pinned: Some(true),
                    mute: None,
                    archived: None,
                },
                false,
            ),
        ]
    }

    #[test]
    fn new_dispatch_is_pending_unless_it_waits_for_a_message() {
        let action = || Action::RenameThread {
            title: "title".into(),
        };
        let ready = Dispatch::new(target(), ConversationId::new(), action(), None);
        assert!(ready.is_pending());

        let parent = MessageId::new();
        let blocked = Dispatch::new(target(), ConversationId::new(), action(), Some(parent));
        assert!(!blocked.is_pending());
        assert_eq!(
            blocked.state,
            DispatchState::Blocked {
                waiting_for: parent
            }
        );
    }

    #[test]
    fn only_sends_edits_and_deletes_record_a_copy() {
        for (action, records) in actions() {
            let dispatch = Dispatch::new(target(), ConversationId::new(), action, None);
            assert_eq!(dispatch.records_copy(), records, "{:?}", dispatch.action);
        }
    }

    #[test]
    fn only_conversation_actions_have_no_message() {
        for (action, _) in actions() {
            let renames = matches!(
                action,
                Action::RenameThread { .. } | Action::Note { .. } | Action::SetChatState { .. }
            );
            let dispatch = Dispatch::new(target(), ConversationId::new(), action, None);
            assert_eq!(
                dispatch.message_id().is_none(),
                renames,
                "{:?}",
                dispatch.action
            );
        }
    }
}
