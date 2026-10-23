//! Outbound delivery: the payload variants, the resolved target, and the
//! persisted dispatch the core tracks to completion.

use crate::*;
use serde::{Deserialize, Serialize};

/// The content of an outbound dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Deliver a new copy. How the copy is rendered (attachments-as-files,
    /// text spillover, and the like) is the target connector's own
    /// configuration, so the dispatch carries only content.
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
    /// Rename an already-provisioned mirror thread when its source
    /// conversation's title changes. Carries no message because a retitle is a
    /// conversation property, not a per-message operation; the target thread is
    /// addressed by the resolved [`TargetAddress`].
    RetitleThread {
        replica_set_id: ReplicaSetId,
        title: String,
    },
    /// Reflect a reaction change on an already-delivered copy. The core only
    /// routes the signal; the connector decides how to render it (e.g. Telegram
    /// shows text rather than a native reaction). `target_platform_message_id`
    /// is the native id of the reacted copy at the target endpoint — empty while
    /// the dispatch is blocked, filled when the parent create is acknowledged.
    Reaction {
        message_id: MessageId,
        replica_set_id: ReplicaSetId,
        target_platform_message_id: String,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
    },
    /// Signal the origin copy that its message reached all of its mirrors. The
    /// connector decides how to surface it (e.g. a 👍 reaction).
    DeliveryAck {
        message_id: MessageId,
        replica_set_id: ReplicaSetId,
        target_platform_message_id: String,
    },
    /// Signal the origin copy that its message was read on a mirror. The
    /// connector decides how to surface it (e.g. a 👀 reaction in DMs).
    SeenReceipt {
        message_id: MessageId,
        replica_set_id: ReplicaSetId,
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
    pub address: ChatAddress,
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
    pub payload: Action,
}

impl DispatchCommand {
    /// The logical message a dispatch carries, or `None` for payloads that
    /// operate on a conversation rather than a message (a thread retitle). The
    /// id keys the delivered copy a later edit/delete revises; a retitle has no
    /// copy to record, so it has no id.
    pub fn message_id(&self) -> Option<MessageId> {
        match &self.payload {
            Action::MessageCreate { message, .. }
            | Action::MessageEdit { message, .. }
            | Action::MessageDelete { message, .. } => Some(message.id),
            Action::Reaction { message_id, .. }
            | Action::DeliveryAck { message_id, .. }
            | Action::SeenReceipt { message_id, .. } => Some(*message_id),
            Action::RetitleThread { .. } => None,
        }
    }

    /// Whether acknowledging this dispatch records a delivered copy. Only the
    /// message-bearing payloads establish (or re-affirm) a copy; reactions,
    /// delivery acks, seen receipts, and retitles act on existing copies and
    /// must not record one — a signal targeting the origin copy would otherwise
    /// flip its received copy to a sent copy.
    pub fn records_copy(&self) -> bool {
        matches!(
            self.payload,
            Action::MessageCreate { .. }
                | Action::MessageEdit { .. }
                | Action::MessageDelete { .. }
        )
    }

    pub fn message_replica_set_id(&self) -> ReplicaSetId {
        match &self.payload {
            Action::MessageCreate { message, .. }
            | Action::MessageEdit { message, .. }
            | Action::MessageDelete { message, .. } => message.replica_set_id,
            Action::Reaction { replica_set_id, .. }
            | Action::DeliveryAck { replica_set_id, .. }
            | Action::SeenReceipt { replica_set_id, .. }
            | Action::RetitleThread { replica_set_id, .. } => *replica_set_id,
        }
    }
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
    Acknowledged { platform_message_id: String },
    /// The connector reported the dispatch can never succeed. It is retained for
    /// observability but is no longer pending, so it is never redelivered.
    Failed { reason: String },
}

/// A [`DispatchCommand`] together with its persisted [`DispatchState`] — one
/// outbound delivery the core tracks to completion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    pub id: DispatchId,
    pub command: DispatchCommand,
    pub state: DispatchState,
}

/// A connector's confirmation that a dispatch was delivered, returned after a
/// successful send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchAck {
    pub dispatch_id: DispatchId,
    pub platform_message_id: String,
    /// The thread/topic the message landed in. Set when the dispatch provisioned
    /// a new thread, so the core can persist the conversation thread mapping.
    #[serde(default)]
    pub thread_id: Option<String>,
}

/// The dispatch commands one routing pass produced: the sendable ones plus the
/// ones blocked behind a message that has not been delivered yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingOutcome {
    pub dispatches: Vec<DispatchCommand>,
    pub blocked: Vec<BlockedDispatch>,
}

/// A dispatch command held until another message is delivered (its reply
/// parent, or the copy an edit/delete/reaction must address by native id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedDispatch {
    pub command: DispatchCommand,
    pub waiting_for_message: MessageId,
}

/// The core's verdict on one ingested connector event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    Routed {
        message_id: MessageId,
        routing: RoutingOutcome,
        dispatches: Vec<Dispatch>,
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
}

/// The sender identity stamped on owner messages. Platforms deliver
/// the message from the owner's own account, so this label only surfaces in the
/// bridge's own records and mirror renderings.
pub const OWNER_AUTHOR: &str = "me";

/// An owner-authored outbound text addressed by raw platform coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerMessage {
    pub connector_id: ConnectorId,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub text: String,
    /// Native id of the message this send replies to, when known. Resolved
    /// best-effort: a reply to a message the bridge never stored sends as a
    /// plain message rather than failing.
    pub reply_to_platform_message_id: Option<String>,
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
