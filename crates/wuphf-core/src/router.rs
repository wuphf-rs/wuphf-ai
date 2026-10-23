//! Fan-out routing: turn a logical create/edit/delete/retitle into the
//! per-endpoint dispatch commands that replicate it, resolving mirror-thread
//! provisioning and blocking dispatches whose reply parent or target copy does
//! not yet exist.

use crate::config::RoutingConfig;
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    Action, ChatAddress, ConnectorId, Conversation, Dispatch, Endpoint, EndpointId, Message,
    MessageId, ReceiptKind, ReplyContext, Rule, RuleKind, Target,
};
use wuphf_storage::{Storage, StorageError};

pub(crate) struct Router<S> {
    storage: Arc<S>,
    config: Arc<RoutingConfig>,
}

impl<S> Router<S>
where
    S: Storage,
{
    pub fn new(storage: Arc<S>, config: Arc<RoutingConfig>) -> Self {
        Self { storage, config }
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
    #[error("origin endpoint {0} is not a member of the replica set")]
    OriginOutsideReplicaSet(EndpointId),
    #[error("reply parent message {0} is not stored")]
    MessageNotFound(MessageId),
}

impl<S> Router<S>
where
    S: Storage,
{
    /// Fan a new message out to every other endpoint in its replica set. A
    /// reply whose parent has no copy at a target yet is blocked on the parent
    /// there, and gains the quote when the parent is delivered.
    pub async fn route(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> Result<Vec<Dispatch>, RouterError> {
        let replica_set = self.storage.replica_set(message.replica_set_id).await?;
        if !replica_set.endpoints.contains(&origin_endpoint) {
            return Err(RouterError::OriginOutsideReplicaSet(origin_endpoint));
        }
        let conversation = self
            .storage
            .conversation_for_replica_set(message.replica_set_id)
            .await?;

        let mut dispatches = Vec::new();
        for endpoint_id in replica_set
            .endpoints
            .into_iter()
            .filter(|endpoint_id| *endpoint_id != origin_endpoint)
        {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            let target = self
                .resolve_target(&endpoint, conversation.as_ref(), provision_title)
                .await?;

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
                message.replica_set_id,
                Action::Send {
                    message: message.clone(),
                    reply,
                },
                waiting_for,
            ));
        }
        Ok(dispatches)
    }

    /// Fan an edit of an already-routed message out to every other endpoint in
    /// the replica set. A target whose copy already exists is dispatched
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
    /// reaction (e.g. as text), so the router only carries the reactor and emoji.
    pub async fn route_reaction(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
    ) -> Result<Vec<Dispatch>, RouterError> {
        self.fan_out_to_copies(message, origin_endpoint, |native_id| Action::React {
            message_id: message.id,
            native_id,
            reactor: reactor.clone(),
            reactor_name: reactor_name.clone(),
            emoji: emoji.clone(),
        })
        .await
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
    /// the replica set. Each target either carries the native id of its copy or
    /// is blocked on the message until that copy is delivered. `action_for`
    /// builds the per-target action from the native id — empty while blocked
    /// and bound on release.
    async fn fan_out_to_copies(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        action_for: impl Fn(String) -> Action,
    ) -> Result<Vec<Dispatch>, RouterError> {
        let replica_set = self.storage.replica_set(message.replica_set_id).await?;
        if !replica_set.endpoints.contains(&origin_endpoint) {
            return Err(RouterError::OriginOutsideReplicaSet(origin_endpoint));
        }

        let mut dispatches = Vec::new();
        for endpoint_id in replica_set
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
                message.replica_set_id,
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
        let replica_set = self
            .storage
            .replica_set(conversation.replica_set_id)
            .await?;
        let Some(rule) = self.config.rule(&conversation.rule_id) else {
            return Ok(Vec::new());
        };
        let mut dispatches = Vec::new();
        for endpoint_id in replica_set.endpoints {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            if !is_mirror_side(rule, &endpoint.connector_id, &endpoint.chat_id) {
                continue;
            }
            let Some(thread) = self
                .storage
                .thread_for_conversation(conversation.id, &endpoint.connector_id)
                .await?
            else {
                continue;
            };
            dispatches.push(Dispatch::new(
                Target {
                    endpoint_id,
                    connector_id: endpoint.connector_id.clone(),
                    address: ChatAddress::new(endpoint.chat_id.clone(), Some(thread)),
                    create_thread: None,
                },
                conversation.replica_set_id,
                Action::RenameThread {
                    title: title.to_owned(),
                },
                None,
            ));
        }
        Ok(dispatches)
    }

    /// Resolve the platform destination for one target endpoint. Mirror
    /// endpoints either reuse their provisioned thread or ask the connector to
    /// create one inside the mirror chat; all other endpoints send to their
    /// canonical address.
    async fn resolve_target(
        &self,
        endpoint: &Endpoint,
        conversation: Option<&Conversation>,
        provision_title: &str,
    ) -> Result<Target, RouterError> {
        let mut target = Target {
            endpoint_id: endpoint.id,
            connector_id: endpoint.connector_id.clone(),
            address: endpoint.address(),
            create_thread: None,
        };
        if let Some(conversation) = conversation
            && let Some(rule) = self.config.rule(&conversation.rule_id)
            && is_mirror_side(rule, &endpoint.connector_id, &endpoint.chat_id)
        {
            let thread = self
                .storage
                .thread_for_conversation(conversation.id, &endpoint.connector_id)
                .await?;
            if thread.is_none() {
                target.create_thread = Some(provision_title.to_owned());
            }
            target.address = ChatAddress::new(endpoint.chat_id.clone(), thread);
        }
        Ok(target)
    }
}
