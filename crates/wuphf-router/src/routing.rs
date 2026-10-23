//! Fan-out routing: turn a logical create/edit/delete/retitle into the
//! per-endpoint dispatch commands that replicate it, resolving mirror-thread
//! provisioning and blocking dispatches whose reply parent or target copy does
//! not yet exist.

use crate::{Router, RouterError, is_mirror_side};
use wuphf_model::{
    Action, BlockedDispatch, ChatAddress, ConnectorId, Conversation, DispatchCommand, Endpoint,
    EndpointId, Message, ProvisionThread, ReceiptKind, ReplyContext, RoutingOutcome, TargetAddress,
};
use wuphf_storage::Storage;

impl<S> Router<S>
where
    S: Storage,
{
    pub async fn route(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> Result<RoutingOutcome, RouterError> {
        let replica_set = self.storage.replica_set(message.replica_set_id).await?;
        if !replica_set.endpoints.contains(&origin_endpoint) {
            return Err(RouterError::OriginOutsideReplicaSet(origin_endpoint));
        }
        let conversation = self
            .storage
            .conversation_for_replica_set(message.replica_set_id)
            .await?;

        let mut outcome = RoutingOutcome::default();
        for endpoint_id in replica_set
            .endpoints
            .into_iter()
            .filter(|endpoint_id| *endpoint_id != origin_endpoint)
        {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            let target = self
                .resolve_target(&endpoint, conversation.as_ref(), provision_title)
                .await?;
            let target_connector = endpoint.connector_id.clone();

            let reply_context = match message.reply_to {
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
                            Some(ReplyContext::quoting(
                                parent,
                                copy.native_id,
                                authored_by_bridge,
                            ))
                        }
                        None => {
                            outcome.blocked.push(BlockedDispatch {
                                command: DispatchCommand {
                                    target_endpoint: endpoint_id,
                                    target_connector: target_connector.clone(),
                                    target: target.clone(),
                                    payload: Action::Send {
                                        message: message.clone(),
                                        reply: None,
                                    },
                                },
                                waiting_for_message: parent_id,
                            });
                            continue;
                        }
                    }
                }
                None => None,
            };

            outcome.dispatches.push(DispatchCommand {
                target_endpoint: endpoint_id,
                target_connector,
                target,
                payload: Action::Send {
                    message: message.clone(),
                    reply: reply_context,
                },
            });
        }
        Ok(outcome)
    }

    /// Fan an edit of an already-routed message out to every other endpoint in
    /// the replica set. A target whose copy already exists is dispatched
    /// immediately, carrying that copy's native id to edit; a target whose
    /// create has not been acknowledged yet is blocked on the message and gains
    /// its native id when the create is acknowledged.
    pub async fn route_edit(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
    ) -> Result<RoutingOutcome, RouterError> {
        self.fan_out_to_copies(message, origin_endpoint, |_connector, native_id| {
            Action::Edit {
                message: message.clone(),
                native_id,
            }
        })
        .await
    }

    pub async fn route_delete(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
    ) -> Result<RoutingOutcome, RouterError> {
        self.fan_out_to_copies(message, origin_endpoint, |_connector, native_id| {
            Action::Delete {
                message: message.clone(),
                native_id,
            }
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
    ) -> Result<RoutingOutcome, RouterError> {
        self.fan_out_to_copies(message, origin_endpoint, |_connector, native_id| {
            Action::React {
                message_id: message.id,
                replica_set_id: message.replica_set_id,
                native_id,
                reactor: reactor.clone(),
                reactor_name: reactor_name.clone(),
                emoji: emoji.clone(),
            }
        })
        .await
    }

    /// Fan a delivery/read receipt out to every other endpoint that holds a
    /// copy of the message. Like a reaction, each target carries the native id
    /// of its copy and is blocked until that copy is delivered. The receipt
    /// kind picks the signal: a delivered receipt becomes a `DeliveryAck`, a
    /// read receipt a `SeenReceipt`; the connector decides how to render each.
    /// Fanning to all non-observer endpoints (rather than only the origin) means
    /// a message the owner authored from another source device — which lands as
    /// a mirror copy on the other endpoints — still surfaces its receipt there.
    pub async fn route_receipt(
        &self,
        message: &Message,
        observer_endpoint: EndpointId,
        kind: ReceiptKind,
    ) -> Result<RoutingOutcome, RouterError> {
        self.fan_out_to_copies(
            message,
            observer_endpoint,
            |_connector, native_id| match kind {
                ReceiptKind::Delivered => Action::DeliveryAck {
                    message_id: message.id,
                    replica_set_id: message.replica_set_id,
                    native_id,
                },
                ReceiptKind::Read => Action::SeenReceipt {
                    message_id: message.id,
                    replica_set_id: message.replica_set_id,
                    native_id,
                },
            },
        )
        .await
    }

    /// Fan an operation on an existing message out to every other endpoint in
    /// the replica set. Each target either carries the native id of its copy or
    /// is blocked on the message until that copy is delivered. `payload_for`
    /// builds the per-target payload from (target connector, native id) — the
    /// native id is empty while blocked and bound on release.
    async fn fan_out_to_copies(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        payload_for: impl Fn(&ConnectorId, String) -> Action,
    ) -> Result<RoutingOutcome, RouterError> {
        let replica_set = self.storage.replica_set(message.replica_set_id).await?;
        if !replica_set.endpoints.contains(&origin_endpoint) {
            return Err(RouterError::OriginOutsideReplicaSet(origin_endpoint));
        }

        let mut outcome = RoutingOutcome::default();
        for endpoint_id in replica_set
            .endpoints
            .into_iter()
            .filter(|endpoint_id| *endpoint_id != origin_endpoint)
        {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            let target = TargetAddress {
                address: endpoint.address(),
                create_thread: None,
            };
            let target_connector = endpoint.connector_id.clone();
            match self
                .storage
                .copy_at_endpoint(message.id, endpoint_id)
                .await?
            {
                Some(copy) => outcome.dispatches.push(DispatchCommand {
                    target_endpoint: endpoint_id,
                    payload: payload_for(&target_connector, copy.native_id),
                    target_connector,
                    target,
                }),
                None => outcome.blocked.push(BlockedDispatch {
                    command: DispatchCommand {
                        target_endpoint: endpoint_id,
                        payload: payload_for(&target_connector, String::new()),
                        target_connector,
                        target,
                    },
                    waiting_for_message: message.id,
                }),
            }
        }
        Ok(outcome)
    }

    /// Fan a source-title change out to every mirror endpoint that has already
    /// provisioned a thread. Endpoints without a thread are skipped: their
    /// thread is created with the current title by the next message that routes
    /// through it, so there is nothing to rename yet.
    pub async fn route_retitle(
        &self,
        conversation: &Conversation,
        title: &str,
    ) -> Result<RoutingOutcome, RouterError> {
        let replica_set = self
            .storage
            .replica_set(conversation.replica_set_id)
            .await?;
        let Some(rule) = self.config.rule(&conversation.rule_id) else {
            return Ok(RoutingOutcome::default());
        };
        let mut outcome = RoutingOutcome::default();
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
            outcome.dispatches.push(DispatchCommand {
                target_endpoint: endpoint_id,
                target_connector: endpoint.connector_id.clone(),
                target: TargetAddress {
                    address: ChatAddress::new(endpoint.chat_id.clone(), Some(thread)),
                    create_thread: None,
                },
                payload: Action::RenameThread {
                    replica_set_id: conversation.replica_set_id,
                    title: title.to_owned(),
                },
            });
        }
        Ok(outcome)
    }

    /// Resolve the platform destination for one target endpoint. Mirror
    /// endpoints either reuse their provisioned thread or carry a provision
    /// instruction; all other endpoints send to their canonical address.
    async fn resolve_target(
        &self,
        endpoint: &Endpoint,
        conversation: Option<&Conversation>,
        provision_title: &str,
    ) -> Result<TargetAddress, RouterError> {
        if let Some(conversation) = conversation
            && let Some(rule) = self.config.rule(&conversation.rule_id)
            && is_mirror_side(rule, &endpoint.connector_id, &endpoint.chat_id)
        {
            return Ok(
                match self
                    .storage
                    .thread_for_conversation(conversation.id, &endpoint.connector_id)
                    .await?
                {
                    Some(thread) => TargetAddress {
                        address: ChatAddress::new(endpoint.chat_id.clone(), Some(thread)),
                        create_thread: None,
                    },
                    None => TargetAddress {
                        address: ChatAddress::new(endpoint.chat_id.clone(), None),
                        create_thread: Some(ProvisionThread {
                            container: endpoint.chat_id.clone(),
                            title: provision_title.to_owned(),
                        }),
                    },
                },
            );
        }
        Ok(TargetAddress {
            address: endpoint.address(),
            create_thread: None,
        })
    }
}
