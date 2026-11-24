//! Fan-out routing: turn a logical create/edit/delete/retitle into the
//! per-endpoint dispatch commands that replicate it, resolving mirror-thread
//! provisioning and blocking dispatches whose reply parent or target copy does
//! not yet exist.

use crate::config::RoutingConfig;
use crate::notes::Note;
use crate::profile::target_disappearing;
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    Action, ConnectorId, Conversation, ConversationId, Dispatch, Endpoint, EndpointId, Message,
    MessageAuthor, MessageId, ReceiptKind, ReplyContext, Rule, RuleKind, Target,
};
use wuphf_storage::{ChatProfileStore, MessageStore, StorageError, TopologyStore};

/// Routes against one operation's routing snapshot; the core builds one per
/// operation (see [`crate::RoutingSource`]).
pub(crate) struct Router<S> {
    storage: Arc<S>,
    routing: Arc<RoutingConfig>,
}

impl<S> Router<S>
where
    S: TopologyStore + MessageStore + ChatProfileStore,
{
    pub fn new(storage: Arc<S>, routing: Arc<RoutingConfig>) -> Self {
        Self { storage, routing }
    }
}

fn is_mirror_side(rule: &Rule, connector: &ConnectorId, container: &str) -> bool {
    match &rule.flow {
        RuleKind::OneToOne { .. } => false,
        RuleKind::ManyToOne {
            destination_connector,
            destination_chat,
            ..
        } => destination_connector == connector && destination_chat.chat_id == container,
    }
}

#[derive(Debug, Error)]
pub enum RouterError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("message conversation {0} is not stored")]
    ConversationNotFound(ConversationId),
    #[error("origin endpoint {0} is not a member of the conversation")]
    OriginOutsideConversation(EndpointId),
    #[error("reply parent message {0} is not stored")]
    MessageNotFound(MessageId),
}

impl<S> Router<S>
where
    S: TopologyStore + MessageStore + ChatProfileStore,
{
    /// Fan a new message out to every other endpoint in its conversation. A
    /// reply whose parent has no copy at a target yet is blocked on the parent
    /// there, and gains the quote when the parent is delivered.
    pub async fn route(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> Result<Vec<Dispatch>, RouterError> {
        let conversation = self.origin_conversation(message, origin_endpoint).await?;

        let mut dispatches = Vec::new();
        for endpoint_id in conversation
            .endpoints
            .iter()
            .copied()
            .filter(|endpoint_id| *endpoint_id != origin_endpoint)
        {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            let target = self
                .resolve_target(&endpoint, &conversation, provision_title)
                .await?;
            let disappearing = target_disappearing(&*self.storage, &self.routing, &target).await?;

            let (reply, waiting_for) = match message.reply_to {
                Some(parent_id) => {
                    match self
                        .storage
                        .copy_at_endpoint(parent_id, endpoint_id)
                        .await?
                    {
                        Some(copy) => {
                            let parent = self
                                .storage
                                .message(parent_id)
                                .await?
                                .ok_or(RouterError::MessageNotFound(parent_id))?;
                            // Whether this copy was sent by the bridge or
                            // received here decides who the target platform is
                            // told wrote the quoted message.
                            let authored_by_bridge = self
                                .storage
                                .is_sent_copy(endpoint_id, &copy.native_id)
                                .await?;
                            let reply =
                                ReplyContext::quoting(parent, copy.native_id, authored_by_bridge);
                            (Some(reply), None)
                        }
                        None => (None, Some(parent_id)),
                    }
                }
                None => (None, None),
            };

            dispatches.push(Dispatch::new(
                target,
                message.conversation_id,
                Action::Send {
                    message: message.clone(),
                    reply,
                    disappearing,
                },
                waiting_for,
            ));
        }
        Ok(dispatches)
    }

    /// Fan an edit of an already-routed message out to every other endpoint in
    /// the conversation. A target whose copy already exists is dispatched
    /// immediately, carrying that copy's native id to edit; a target whose
    /// send has not been acknowledged yet is blocked on the message and gains
    /// its native id when the send is acknowledged.
    pub async fn route_edit(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
    ) -> Result<Vec<Dispatch>, RouterError> {
        self.fan_out_to_copies(message, origin_endpoint, |native_id| Action::Edit {
            message: message.clone(),
            native_id,
        })
        .await
    }

    pub async fn route_delete(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
    ) -> Result<Vec<Dispatch>, RouterError> {
        self.fan_out_to_copies(message, origin_endpoint, |native_id| Action::Delete {
            message_id: message.id,
            native_id,
        })
        .await
    }

    /// Fan a reaction change out to the other endpoints. Like an edit, each
    /// target carries the native id of the copy the reaction applies to, blocked
    /// until that copy is delivered. The connector decides how to render the
    /// reaction (e.g. as text, or natively when `by_owner`), so the router
    /// carries the reactor, the emoji, and who wrote each target's copy: a
    /// delivered copy's author is known now, a blocked one's when it is released.
    pub async fn route_reaction(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
        by_owner: bool,
    ) -> Result<Vec<Dispatch>, RouterError> {
        let mut dispatches = self
            .fan_out_to_copies(message, origin_endpoint, |native_id| Action::React {
                message_id: message.id,
                native_id,
                reactor: reactor.clone(),
                reactor_name: reactor_name.clone(),
                emoji: emoji.clone(),
                by_owner,
                target_author: MessageAuthor::default(),
            })
            .await?;
        for dispatch in &mut dispatches {
            let endpoint_id = dispatch.to.endpoint_id;
            if let Action::React {
                native_id,
                target_author,
                ..
            } = &mut dispatch.action
                && !native_id.is_empty()
            {
                let authored_by_bridge = self.storage.is_sent_copy(endpoint_id, native_id).await?;
                *target_author = MessageAuthor::of(message, authored_by_bridge);
            }
        }
        Ok(dispatches)
    }

    /// Fan a delivery/read receipt out to every other endpoint that holds a
    /// copy of the message. Like a reaction, each target carries the native id
    /// of its copy and is blocked until that copy is delivered; the connector
    /// decides how to render each receipt kind. Fanning to all non-observer
    /// endpoints (rather than only the origin) means a message the owner
    /// authored from another source device — which lands as a mirror copy on
    /// the other endpoints — still surfaces its receipt there.
    pub async fn route_receipt(
        &self,
        message: &Message,
        observer_endpoint: EndpointId,
        kind: ReceiptKind,
    ) -> Result<Vec<Dispatch>, RouterError> {
        self.fan_out_to_copies(message, observer_endpoint, |native_id| Action::Receipt {
            message_id: message.id,
            native_id,
            kind,
        })
        .await
    }

    /// Fan an operation on an existing message out to every other endpoint in
    /// the conversation. Each target either carries the native id of its copy or
    /// is blocked on the message until that copy is delivered. `action_for`
    /// builds the per-target action from the native id — empty while blocked
    /// and bound on release.
    async fn fan_out_to_copies(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        action_for: impl Fn(String) -> Action,
    ) -> Result<Vec<Dispatch>, RouterError> {
        let conversation = self.origin_conversation(message, origin_endpoint).await?;

        let mut dispatches = Vec::new();
        for endpoint_id in conversation
            .endpoints
            .into_iter()
            .filter(|endpoint_id| *endpoint_id != origin_endpoint)
        {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            let target = Target {
                endpoint_id,
                connector_id: endpoint.connector_id.clone(),
                address: endpoint.address(),
                create_thread: None,
            };
            let (native_id, waiting_for) = match self
                .storage
                .copy_at_endpoint(message.id, endpoint_id)
                .await?
            {
                Some(copy) => (copy.native_id, None),
                None => (String::new(), Some(message.id)),
            };
            dispatches.push(Dispatch::new(
                target,
                message.conversation_id,
                action_for(native_id),
                waiting_for,
            ));
        }
        Ok(dispatches)
    }

    /// Fan a source-title change out to every mirror endpoint that has already
    /// provisioned a thread. Endpoints without a thread are skipped: their
    /// thread is created with the current title by the next message that routes
    /// through it, so there is nothing to rename yet.
    pub async fn route_retitle(
        &self,
        conversation: &Conversation,
        title: &str,
    ) -> Result<Vec<Dispatch>, RouterError> {
        let mut dispatches = Vec::new();
        for endpoint_id in &conversation.endpoints {
            let endpoint = self.storage.endpoint(*endpoint_id).await?;
            if endpoint.thread_id.is_none() || !self.is_mirror(conversation, &endpoint) {
                continue;
            }
            dispatches.push(Dispatch::new(
                Target {
                    endpoint_id: endpoint.id,
                    connector_id: endpoint.connector_id.clone(),
                    address: endpoint.address(),
                    create_thread: None,
                },
                conversation.id,
                Action::RenameThread {
                    title: title.to_owned(),
                },
                None,
            ));
        }
        Ok(dispatches)
    }

    /// Post a system note to every endpoint of the conversation except `from`
    /// (the chat it is about). A mirror endpoint without a thread is skipped:
    /// its info card will show the current state when the thread is created.
    pub async fn route_note(
        &self,
        conversation: &Conversation,
        from: EndpointId,
        note: &Note,
    ) -> Result<Vec<Dispatch>, RouterError> {
        let mut dispatches = Vec::new();
        for endpoint_id in conversation
            .endpoints
            .iter()
            .copied()
            .filter(|endpoint_id| *endpoint_id != from)
        {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            if endpoint.thread_id.is_none() && self.is_mirror(conversation, &endpoint) {
                continue;
            }
            dispatches.push(Dispatch::new(
                Target {
                    endpoint_id,
                    connector_id: endpoint.connector_id.clone(),
                    address: endpoint.address(),
                    create_thread: None,
                },
                conversation.id,
                Action::Note {
                    text: note.text.clone(),
                    attachments: note.attachments.clone(),
                },
                None,
            ));
        }
        Ok(dispatches)
    }

    /// The conversation a message belongs to, checked to contain the endpoint
    /// it is being routed from.
    async fn origin_conversation(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
    ) -> Result<Conversation, RouterError> {
        let conversation = self
            .storage
            .conversation(message.conversation_id)
            .await?
            .ok_or(RouterError::ConversationNotFound(message.conversation_id))?;
        if !conversation.endpoints.contains(&origin_endpoint) {
            return Err(RouterError::OriginOutsideConversation(origin_endpoint));
        }
        Ok(conversation)
    }

    /// Whether the endpoint is the mirror side of its conversation's rule: a
    /// many-to-one destination, where each conversation gets its own thread.
    pub(crate) fn is_mirror(&self, conversation: &Conversation, endpoint: &Endpoint) -> bool {
        conversation
            .rule_id
            .as_ref()
            .and_then(|rule_id| self.routing.rule(rule_id))
            .is_some_and(|rule| is_mirror_side(rule, &endpoint.connector_id, &endpoint.chat_id))
    }

    /// Resolve the platform destination for one target endpoint. A mirror
    /// endpoint reuses the thread stamped on it once provisioned, or asks the
    /// connector to create one inside the mirror chat; every other endpoint
    /// sends to its canonical address.
    async fn resolve_target(
        &self,
        endpoint: &Endpoint,
        conversation: &Conversation,
        provision_title: &str,
    ) -> Result<Target, RouterError> {
        let needs_thread = endpoint.thread_id.is_none() && self.is_mirror(conversation, endpoint);
        Ok(Target {
            endpoint_id: endpoint.id,
            connector_id: endpoint.connector_id.clone(),
            address: endpoint.address(),
            create_thread: needs_thread.then(|| provision_title.to_owned()),
        })
    }
}
