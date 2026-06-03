//! Outbound delivery: the payload variants, the resolved target, and the
//! persisted dispatch the core tracks to completion.

use crate::*;
use serde::{Deserialize, Serialize};

/// The content of an outbound dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchPayload {
    MessageCreate {
        message: Message,
        reply_context: Option<ReplyContext>,
        /// The target connector's delivery policy, resolved at routing time, that
        /// governs how this copy is rendered (e.g. attachments-as-files, text
        /// length spillover). Carried whole so new create-time knobs are additive.
        #[serde(default)]
        delivery: DeliveryOptions,
    },
    /// Revise an already-delivered copy. `target_platform_message_id` is the
    /// native id of the copy to edit at the target endpoint; it is empty while
    /// the dispatch is blocked and filled in when the original create is
    /// acknowledged.
    MessageEdit {
        message: Message,
        target_platform_message_id: String,
        /// How the target connector should render the edit, resolved from the
        /// target connector's delivery options at routing time.
        #[serde(default)]
        mode: EditMode,
    },
    /// Revoke an already-delivered copy. `target_platform_message_id` is the
    /// native id of the copy to delete at the target endpoint; it is empty while
    /// the dispatch is blocked and filled in when the original create is
    /// acknowledged.
    MessageDelete {
        message: Message,
        target_platform_message_id: String,
        /// How the target connector should render the delete, resolved from the
        /// target connector's delivery options at routing time.
        #[serde(default)]
        mode: DeleteMode,
    },
    /// Rename an already-provisioned mirror thread when its source
    /// conversation's title changes. Carries no message because a retitle is a
    /// conversation property, not a per-message operation; the target thread is
    /// addressed by the resolved [`TargetAddress`].
    RetitleThread {
        replica_set_id: ReplicaSetId,
        title: String,
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
    /// The logical message a dispatch carries, or `None` for payloads that
    /// operate on a conversation rather than a message (a thread retitle). The
    /// id keys the delivered copy a later edit/delete revises; a retitle has no
    /// copy to record, so it has no id.
    pub fn message_id(&self) -> Option<MessageId> {
        match &self.payload {
            DispatchPayload::MessageCreate { message, .. }
            | DispatchPayload::MessageEdit { message, .. }
            | DispatchPayload::MessageDelete { message, .. } => Some(message.id),
            DispatchPayload::RetitleThread { .. } => None,
        }
    }

    pub fn message_replica_set_id(&self) -> ReplicaSetId {
        match &self.payload {
            DispatchPayload::MessageCreate { message, .. }
            | DispatchPayload::MessageEdit { message, .. }
            | DispatchPayload::MessageDelete { message, .. } => message.replica_set_id,
            DispatchPayload::RetitleThread { replica_set_id, .. } => *replica_set_id,
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
