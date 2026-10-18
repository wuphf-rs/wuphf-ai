//! The minimum surface every connector implements, and how delivery failures
//! are classified. This mirrors `connectors/shared/coreclient` on the Go side:
//! permanent failures are reported to the core for dead-lettering, transient
//! ones end the session so the core redelivers the still-unacknowledged
//! dispatch on reconnect.

use std::future::Future;

use wuphf_proto::connector as pb;

/// A dispatch delivery failure. Connectors return [`Failure::Permanent`] for
/// failures that can never succeed on retry (e.g. an unparseable target chat);
/// anything else is transient and is surfaced by dropping the session, which
/// makes the core redeliver the dispatch rather than dead-lettering it.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
    #[error("{0}")]
    Permanent(String),
    #[error(transparent)]
    Transient(#[from] anyhow::Error),
}

impl Failure {
    pub fn permanent(message: impl Into<String>) -> Self {
        Self::Permanent(message.into())
    }

    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }
}

/// The result of a successful message create: the platform message id, plus
/// the thread id when the send provisioned a new thread, so the core can
/// persist the conversation-to-thread mapping.
#[derive(Debug)]
pub struct Delivery {
    pub message_id: String,
    pub thread_id: Option<String>,
}

/// The dispatch surface a connector implements. Create/edit/delete are
/// required; the capability operations have defaults matching the Go
/// coreclient's fallback behaviour: an unsupported retitle is dead-lettered
/// (the core only routes retitles to mirror-capable connectors, so this is a
/// safety net), while reactions, delivery acks and seen receipts are
/// acknowledged unchanged so the core stops redelivering them.
pub trait Sender: Send + Sync + 'static {
    /// Deliver a message-create dispatch and return the resulting platform
    /// message id (and provisioned thread id, when one was created).
    fn send_message(
        &self,
        dispatch: pb::MessageCreateDispatch,
    ) -> impl Future<Output = Result<Delivery, Failure>> + Send;

    /// Revise an already-delivered copy, returning its platform message id,
    /// which an edit leaves unchanged.
    fn edit_message(
        &self,
        dispatch: pb::MessageEditDispatch,
    ) -> impl Future<Output = Result<String, Failure>> + Send;

    /// Revoke an already-delivered copy, returning its platform message id,
    /// which a delete leaves unchanged.
    fn delete_message(
        &self,
        dispatch: pb::MessageDeleteDispatch,
    ) -> impl Future<Output = Result<String, Failure>> + Send;

    /// Rename an already-provisioned mirror thread, returning the thread id,
    /// which the retitle leaves unchanged.
    fn retitle_thread(
        &self,
        _dispatch: pb::MessageRetitleDispatch,
    ) -> impl Future<Output = Result<String, Failure>> + Send {
        async move {
            Err(Failure::permanent(
                "connector does not support thread retitling",
            ))
        }
    }

    /// Reflect a reaction change on an already-delivered copy, returning the
    /// reacted copy's platform message id unchanged so the reaction does not
    /// disturb the copy mapping.
    fn apply_reaction(
        &self,
        dispatch: pb::ReactionDispatch,
    ) -> impl Future<Output = Result<String, Failure>> + Send {
        async move { Ok(dispatch.target_platform_message_id) }
    }

    /// Surface that a sent message reached the source, returning the target
    /// copy's platform message id unchanged.
    fn ack_delivery(
        &self,
        dispatch: pb::DeliveryAckDispatch,
    ) -> impl Future<Output = Result<String, Failure>> + Send {
        async move { Ok(dispatch.target_platform_message_id) }
    }

    /// Surface that a sent message was read, returning the target copy's
    /// platform message id unchanged.
    fn mark_seen(
        &self,
        dispatch: pb::SeenReceiptDispatch,
    ) -> impl Future<Output = Result<String, Failure>> + Send {
        async move { Ok(dispatch.target_platform_message_id) }
    }
}
