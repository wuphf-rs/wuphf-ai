use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    ConnectorCapabilities, ConnectorId, Conversation, DeliveryOptions, DispatchCommand,
    DispatchPayload, Endpoint, EndpointId, Message, MessageId, Platform, PlatformAddress,
    ProvisionThread, ReplyContext, Rule, RuleId, SideKind, TargetAddress,
};
use wuphf_storage::{Storage, StorageError};

/// Static per-connector facts the core learns at registration: the platform it
/// speaks and the capabilities it advertises (used to gate mirror sides).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorInfo {
    pub platform: Platform,
    pub capabilities: ConnectorCapabilities,
    /// Server-owned policy for how this connector renders replicated
    /// operations on its platform (e.g. edit/delete rendering).
    pub delivery: DeliveryOptions,
}

/// The rule set plus connector metadata that drives address resolution and
/// thread provisioning. Shared by the core and the router.
pub struct RoutingConfig {
    rules: Vec<Rule>,
    connectors: HashMap<ConnectorId, ConnectorInfo>,
}

/// The side of a rule that an inbound address matched.
pub struct MatchedSide<'a> {
    pub rule: &'a Rule,
    pub origin_side: usize,
}

impl RoutingConfig {
    pub fn new(rules: Vec<Rule>, connectors: HashMap<ConnectorId, ConnectorInfo>) -> Self {
        Self { rules, connectors }
    }

    pub fn rule(&self, id: &RuleId) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == *id)
    }

    pub fn connector(&self, id: &ConnectorId) -> Option<&ConnectorInfo> {
        self.connectors.get(id)
    }

    /// The delivery policy configured for a connector, or the default when the
    /// connector is unknown or left unconfigured.
    pub fn delivery(&self, id: &ConnectorId) -> DeliveryOptions {
        self.connector(id).map(|info| info.delivery).unwrap_or_default()
    }

    /// The first rule side whose connector and selector accept this address.
    pub fn match_source(
        &self,
        connector: &ConnectorId,
        address: &PlatformAddress,
    ) -> Option<MatchedSide<'_>> {
        for rule in &self.rules {
            for (index, side) in rule.sides.iter().enumerate() {
                if side.connector != *connector {
                    continue;
                }
                if side_accepts(&side.kind, address) {
                    return Some(MatchedSide {
                        rule,
                        origin_side: index,
                    });
                }
            }
        }
        None
    }
}

/// Whether a side's selector accepts a platform address.
pub fn side_accepts(kind: &SideKind, address: &PlatformAddress) -> bool {
    match kind {
        SideKind::Single { chat_id, thread_id } => {
            address.chat_id == *chat_id && address.thread_id == *thread_id
        }
        SideKind::Source { include, exclude } => {
            (include.is_empty() || include.contains(&address.chat_id))
                && !exclude.contains(&address.chat_id)
        }
        SideKind::Mirror { container } => address.chat_id == *container,
    }
}

pub struct Router<S> {
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
                            Some(reply_context(parent, copy.platform_message_id))
                        }
                        None => {
                            outcome.blocked.push(BlockedDispatch {
                                command: DispatchCommand {
                                    target_endpoint: endpoint_id,
                                    target_connector: target_connector.clone(),
                                    target: target.clone(),
                                    payload: DispatchPayload::MessageCreate {
                                        message: message.clone(),
                                        reply_context: None,
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
                payload: DispatchPayload::MessageCreate {
                    message: message.clone(),
                    reply_context,
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
                provision_thread: None,
            };
            let target_connector = endpoint.connector_id.clone();
            let mode = self.config.delivery(&target_connector).edit_mode;
            match self
                .storage
                .copy_at_endpoint(message.id, endpoint_id)
                .await?
            {
                Some(copy) => outcome.dispatches.push(DispatchCommand {
                    target_endpoint: endpoint_id,
                    target_connector,
                    target,
                    payload: DispatchPayload::MessageEdit {
                        message: message.clone(),
                        target_platform_message_id: copy.platform_message_id,
                        mode,
                    },
                }),
                None => outcome.blocked.push(BlockedDispatch {
                    command: DispatchCommand {
                        target_endpoint: endpoint_id,
                        target_connector,
                        target,
                        payload: DispatchPayload::MessageEdit {
                            message: message.clone(),
                            target_platform_message_id: String::new(),
                            mode,
                        },
                    },
                    waiting_for_message: message.id,
                }),
            }
        }
        Ok(outcome)
    }

    pub async fn route_delete(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
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
                provision_thread: None,
            };
            let target_connector = endpoint.connector_id.clone();
            let mode = self.config.delivery(&target_connector).delete_mode;
            match self
                .storage
                .copy_at_endpoint(message.id, endpoint_id)
                .await?
            {
                Some(copy) => outcome.dispatches.push(DispatchCommand {
                    target_endpoint: endpoint_id,
                    target_connector,
                    target,
                    payload: DispatchPayload::MessageDelete {
                        message: message.clone(),
                        target_platform_message_id: copy.platform_message_id,
                        mode,
                    },
                }),
                None => outcome.blocked.push(BlockedDispatch {
                    command: DispatchCommand {
                        target_endpoint: endpoint_id,
                        target_connector,
                        target,
                        payload: DispatchPayload::MessageDelete {
                            message: message.clone(),
                            target_platform_message_id: String::new(),
                            mode,
                        },
                    },
                    waiting_for_message: message.id,
                }),
            }
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
                        address: PlatformAddress::new(endpoint.chat_id.clone(), Some(thread)),
                        provision_thread: None,
                    },
                    None => TargetAddress {
                        address: PlatformAddress::new(endpoint.chat_id.clone(), None),
                        provision_thread: Some(ProvisionThread {
                            container: endpoint.chat_id.clone(),
                            title: provision_title.to_owned(),
                        }),
                    },
                },
            );
        }
        Ok(TargetAddress {
            address: endpoint.address(),
            provision_thread: None,
        })
    }
}

fn is_mirror_side(rule: &Rule, connector: &ConnectorId, container: &str) -> bool {
    rule.sides.iter().any(|side| {
        side.connector == *connector
            && matches!(&side.kind, SideKind::Mirror { container: c } if c == container)
    })
}

fn reply_context(message: Message, platform_message_id: String) -> ReplyContext {
    ReplyContext {
        message_id: message.id,
        platform_message_id,
        sender: message.sender,
        participant: message.participant,
        text: message.text,
        attachments: message.attachments,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingOutcome {
    pub dispatches: Vec<DispatchCommand>,
    pub blocked: Vec<BlockedDispatch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedDispatch {
    pub command: DispatchCommand,
    pub waiting_for_message: MessageId,
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
