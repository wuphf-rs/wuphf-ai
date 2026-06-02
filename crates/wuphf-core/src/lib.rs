use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use wuphf_model::{
    Alias, ConnectorId, Conversation, ConversationId, DedupKey, DispatchAcknowledgement,
    DispatchId, Endpoint, EndpointId, EventPayload, MediaId, Message, MessageCopy, MessageId,
    NormalizedEvent, PlatformAddress, ReplicaSet, ReplicaSetId, Rule, SideKind, SourceAddress,
    StoredDispatch,
};
use wuphf_router::{Router, RouterError, RoutingOutcome};
use wuphf_storage::{Storage, StorageError};

pub use wuphf_router::{ConnectorInfo, RoutingConfig};

pub struct Core<S> {
    storage: Arc<S>,
    router: Router<S>,
    config: Arc<RoutingConfig>,
    /// Conversations whose mirror thread is mid-provisioning, keyed by replica
    /// set. Guards against a burst of messages each provisioning a duplicate
    /// thread before the first provision is acknowledged. The lock is never
    /// held across an await.
    provisioning: Mutex<HashMap<ReplicaSetId, ProvisionState>>,
}

/// A conversation's in-flight thread provisioning: the dispatches whose
/// acknowledgement establishes the thread, and the messages held until it does.
#[derive(Default)]
struct ProvisionState {
    pending: HashSet<DispatchId>,
    stash: Vec<StashedMessage>,
}

/// A message received while its conversation's mirror thread was still being
/// provisioned, replayed once the thread exists.
struct StashedMessage {
    message: Message,
    origin_endpoint: EndpointId,
    provision_title: String,
}

impl<S> Core<S>
where
    S: Storage,
{
    pub fn new(storage: Arc<S>, config: Arc<RoutingConfig>) -> Self {
        Self {
            router: Router::new(Arc::clone(&storage), Arc::clone(&config)),
            config,
            storage,
            provisioning: Mutex::new(HashMap::new()),
        }
    }

    pub async fn ingest(&self, event: NormalizedEvent) -> Result<IngestOutcome, CoreError> {
        let source = event.source;
        let Some(origin_endpoint) = self.resolve_origin(&source).await? else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let origin = self.storage.endpoint(origin_endpoint).await?;
        let replica_set_id = self
            .storage
            .replica_set_for_endpoint(origin_endpoint)
            .await?
            .ok_or(CoreError::OriginWithoutReplicaSet(origin_endpoint))?;

        match event.payload {
            EventPayload::MessageCreate(inbound) => {
                self.ingest_create(source, origin_endpoint, origin, replica_set_id, inbound)
                    .await
            }
            EventPayload::MessageEdit {
                platform_message_id,
                text,
            } => {
                self.ingest_edit(origin_endpoint, platform_message_id, text)
                    .await
            }
            EventPayload::MessageDelete {
                platform_message_id,
            } => {
                self.ingest_delete(origin_endpoint, platform_message_id)
                    .await
            }
            EventPayload::ReactionAdd(_) => Err(CoreError::UnsupportedEvent),
        }
    }

    async fn ingest_create(
        &self,
        source: SourceAddress,
        origin_endpoint: EndpointId,
        origin: Endpoint,
        replica_set_id: ReplicaSetId,
        inbound: wuphf_model::InboundMessage,
    ) -> Result<IngestOutcome, CoreError> {
        if self
            .storage
            .is_sent_copy(origin_endpoint, &inbound.platform_message_id)
            .await?
        {
            return Ok(IngestOutcome::Ignored(IgnoreReason::SentMessageEcho));
        }

        // Resolve the reply parent best-effort: a reply to a message the bridge
        // never saw (sent before startup, or never mirrored) simply loses its
        // reply linkage rather than failing the whole message.
        let reply_to = match inbound.reply_to_platform_message_id.as_deref() {
            Some(platform_message_id) => {
                self.storage
                    .message_id_for_copy(origin_endpoint, platform_message_id)
                    .await?
            }
            None => None,
        };

        for attachment in &inbound.attachments {
            let media = self
                .storage
                .media(attachment.media_id)
                .await?
                .ok_or(CoreError::UnknownMedia(attachment.media_id))?;
            if media.attachment != *attachment {
                return Err(CoreError::MediaMetadataMismatch(attachment.media_id));
            }
        }

        let dedup_key = DedupKey {
            platform: origin.platform.clone(),
            chat_id: origin.chat_id.clone(),
            platform_message_id: inbound.platform_message_id.clone(),
        };
        if !self.storage.claim_event(dedup_key).await? {
            return Ok(IngestOutcome::Ignored(IgnoreReason::DuplicateEvent));
        }

        let message = Message {
            id: MessageId::new(),
            replica_set_id,
            sender: inbound.sender,
            participant: inbound.participant,
            sender_name: inbound.sender_name,
            text: inbound.text,
            attachments: inbound.attachments,
            reply_to,
            timestamp_ms: inbound.timestamp_ms,
            deleted: false,
        };
        self.storage.put_message(message.clone()).await?;
        self.storage
            .put_received_copy(MessageCopy {
                message_id: message.id,
                endpoint_id: origin_endpoint,
                platform_message_id: inbound.platform_message_id,
            })
            .await?;

        let provision_title = source
            .chat_title
            .clone()
            .unwrap_or_else(|| source.address.chat_id.clone());

        // Hold the message if this conversation's mirror thread is still being
        // provisioned; it is replayed once the thread exists. This prevents a
        // burst of messages from each provisioning a duplicate thread.
        let stashed = {
            let mut provisioning = self.provisioning.lock().unwrap();
            match provisioning.get_mut(&replica_set_id) {
                Some(state) if !state.pending.is_empty() => {
                    state.stash.push(StashedMessage {
                        message: message.clone(),
                        origin_endpoint,
                        provision_title: provision_title.clone(),
                    });
                    true
                }
                _ => false,
            }
        };
        if stashed {
            return Ok(IngestOutcome::Deferred {
                message_id: message.id,
            });
        }

        let (routing, dispatches) = self
            .route_and_record(&message, origin_endpoint, &provision_title)
            .await?;
        Ok(IngestOutcome::Routed {
            message_id: message.id,
            routing,
            dispatches,
        })
    }

    /// Route a message, enqueue its dispatches, and register any thread
    /// provisioning it triggers so that concurrent messages to the same
    /// conversation are stashed (rather than provisioning again) until the
    /// thread exists. Returns the routing and the immediately-sendable
    /// dispatches.
    async fn route_and_record(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> Result<(RoutingOutcome, Vec<StoredDispatch>), CoreError> {
        let routing = self
            .router
            .route(message, origin_endpoint, provision_title)
            .await?;
        let mut dispatches = Vec::new();
        let mut provisioning_ids = Vec::new();
        for command in routing.dispatches.iter().cloned() {
            let provisions = command.target.provision_thread.is_some();
            let stored = self.storage.enqueue_dispatch(command, None).await?;
            if provisions {
                provisioning_ids.push(stored.id);
            }
            dispatches.push(stored);
        }
        for blocked in routing.blocked.iter().cloned() {
            self.storage
                .enqueue_dispatch(blocked.command, Some(blocked.waiting_for_message))
                .await?;
        }
        if !provisioning_ids.is_empty() {
            let mut provisioning = self.provisioning.lock().unwrap();
            provisioning
                .entry(message.replica_set_id)
                .or_default()
                .pending
                .extend(provisioning_ids);
        }
        Ok((routing, dispatches))
    }

    /// Apply an inbound edit: revise the canonical message text and fan the
    /// change out to the other endpoints. An edit whose text matches the stored
    /// text is dropped, which both ignores redundant reports and breaks the echo
    /// loop when our own mirrored edit is reported back by the target platform.
    async fn ingest_edit(
        &self,
        origin_endpoint: EndpointId,
        platform_message_id: String,
        text: Option<String>,
    ) -> Result<IngestOutcome, CoreError> {
        let Some(message_id) = self
            .storage
            .message_id_for_copy(origin_endpoint, &platform_message_id)
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let mut message = self
            .storage
            .message(message_id)
            .await?
            .ok_or(CoreError::EditTargetNotStored(message_id))?;
        if message.text == text {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantEdit));
        }
        message.text = text;
        self.storage.put_message(message.clone()).await?;

        let routing = self.router.route_edit(&message, origin_endpoint).await?;
        let mut dispatches = Vec::new();
        for command in routing.dispatches.iter().cloned() {
            dispatches.push(self.storage.enqueue_dispatch(command, None).await?);
        }
        for blocked in routing.blocked.iter().cloned() {
            self.storage
                .enqueue_dispatch(blocked.command, Some(blocked.waiting_for_message))
                .await?;
        }
        Ok(IngestOutcome::Routed {
            message_id,
            routing,
            dispatches,
        })
    }

    /// Apply an inbound delete: mark the canonical message revoked and fan the
    /// deletion out to the other endpoints. A delete of an already-revoked
    /// message is dropped, which both ignores redundant reports and breaks the
    /// echo loop when our own mirrored delete is reported back by the target.
    async fn ingest_delete(
        &self,
        origin_endpoint: EndpointId,
        platform_message_id: String,
    ) -> Result<IngestOutcome, CoreError> {
        let Some(message_id) = self
            .storage
            .message_id_for_copy(origin_endpoint, &platform_message_id)
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let mut message = self
            .storage
            .message(message_id)
            .await?
            .ok_or(CoreError::DeleteTargetNotStored(message_id))?;
        if message.deleted {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantDelete));
        }
        message.deleted = true;
        self.storage.put_message(message.clone()).await?;

        let routing = self.router.route_delete(&message, origin_endpoint).await?;
        let mut dispatches = Vec::new();
        for command in routing.dispatches.iter().cloned() {
            dispatches.push(self.storage.enqueue_dispatch(command, None).await?);
        }
        for blocked in routing.blocked.iter().cloned() {
            self.storage
                .enqueue_dispatch(blocked.command, Some(blocked.waiting_for_message))
                .await?;
        }
        Ok(IngestOutcome::Routed {
            message_id,
            routing,
            dispatches,
        })
    }

    /// Resolve a raw inbound address to its endpoint, lazily minting the
    /// endpoint, conversation, and replica set on first sighting when a
    /// rule matches. Returns `None` when the address matches no rule, or matches
    /// only a mirror side with no provisioned thread (an unmapped topic).
    async fn resolve_origin(
        &self,
        source: &SourceAddress,
    ) -> Result<Option<EndpointId>, CoreError> {
        if let Some(endpoint) = self
            .storage
            .resolve_endpoint(&source.connector_id, &source.address)
            .await?
        {
            return Ok(Some(endpoint));
        }

        let Some(matched) = self
            .config
            .match_source(&source.connector_id, &source.address)
        else {
            return Ok(None);
        };
        let rule = matched.rule.clone();
        if matches!(
            rule.sides[matched.origin_side].kind,
            SideKind::Mirror { .. }
        ) {
            return Ok(None);
        }

        let identity_side = identity_side_index(&rule);
        let identity_address = match &rule.sides[identity_side].kind {
            SideKind::Single { chat_id, thread_id } => {
                PlatformAddress::new(chat_id.clone(), thread_id.clone())
            }
            SideKind::Source { .. } => source.address.clone(),
            SideKind::Mirror { .. } => unreachable!("identity side is never a mirror"),
        };

        let conversation = self
            .ensure_conversation(&rule, identity_side, &identity_address)
            .await?;

        let mut origin = None;
        for index in 0..rule.sides.len() {
            let endpoint = self
                .ensure_side_endpoint(&conversation, &rule, index, &identity_address)
                .await?;
            if index == matched.origin_side {
                origin = Some(endpoint);
            }
        }
        Ok(origin)
    }

    /// Get the conversation backing this rule's identity address, creating it
    /// (with an empty replica set) on first sighting.
    async fn ensure_conversation(
        &self,
        rule: &Rule,
        identity_side: usize,
        identity_address: &PlatformAddress,
    ) -> Result<Conversation, CoreError> {
        let identity_connector = rule.sides[identity_side].connector.clone();
        if let Some(existing) = self
            .storage
            .conversation_for_source(&rule.id, &identity_connector, identity_address)
            .await?
        {
            return Ok(existing);
        }
        let replica_set_id = ReplicaSetId::new();
        self.storage
            .put_replica_set(ReplicaSet {
                id: replica_set_id,
                endpoints: Vec::new(),
            })
            .await?;
        let conversation = Conversation {
            id: ConversationId::new(),
            rule_id: rule.id.clone(),
            replica_set_id,
            source_connector: identity_connector,
            source_address: identity_address.clone(),
        };
        self.storage.put_conversation(conversation.clone()).await?;
        Ok(conversation)
    }

    /// Get or mint the endpoint for one rule side, joining it to the
    /// conversation's replica set. Single and source sides record a canonical alias;
    /// a mirror side gets no alias until its thread is provisioned.
    async fn ensure_side_endpoint(
        &self,
        conversation: &Conversation,
        rule: &Rule,
        side_index: usize,
        identity_address: &PlatformAddress,
    ) -> Result<EndpointId, CoreError> {
        let side = &rule.sides[side_index];
        let (address, with_alias) = match &side.kind {
            SideKind::Single { chat_id, thread_id } => (
                PlatformAddress::new(chat_id.clone(), thread_id.clone()),
                true,
            ),
            SideKind::Source { .. } => (identity_address.clone(), true),
            SideKind::Mirror { container } => {
                (PlatformAddress::new(container.clone(), None), false)
            }
        };

        if with_alias {
            if let Some(endpoint) = self
                .storage
                .resolve_endpoint(&side.connector, &address)
                .await?
            {
                return Ok(endpoint);
            }
        } else if let Some(endpoint) = self
            .endpoint_in_replica_set(conversation.replica_set_id, &side.connector)
            .await?
        {
            return Ok(endpoint);
        }

        let info = self
            .config
            .connector(&side.connector)
            .ok_or_else(|| CoreError::UnknownConnector(side.connector.clone()))?;
        let endpoint = Endpoint {
            id: EndpointId::new(),
            connector_id: side.connector.clone(),
            platform: info.platform.clone(),
            chat_id: address.chat_id.clone(),
            thread_id: address.thread_id.clone(),
        };
        self.storage.put_endpoint(endpoint.clone()).await?;
        if with_alias {
            self.storage
                .put_alias(Alias {
                    connector_id: side.connector.clone(),
                    address,
                    endpoint_id: endpoint.id,
                    is_canonical: true,
                })
                .await?;
        }
        let mut replica_set = self
            .storage
            .replica_set(conversation.replica_set_id)
            .await?;
        if !replica_set.endpoints.contains(&endpoint.id) {
            replica_set.endpoints.push(endpoint.id);
            self.storage.put_replica_set(replica_set).await?;
        }
        Ok(endpoint.id)
    }

    async fn endpoint_in_replica_set(
        &self,
        replica_set_id: ReplicaSetId,
        connector: &ConnectorId,
    ) -> Result<Option<EndpointId>, CoreError> {
        let replica_set = self.storage.replica_set(replica_set_id).await?;
        for endpoint_id in replica_set.endpoints {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            if endpoint.connector_id == *connector {
                return Ok(Some(endpoint_id));
            }
        }
        Ok(None)
    }

    pub async fn record_dispatched_copy(
        &self,
        message_id: MessageId,
        target_endpoint: EndpointId,
        platform_message_id: impl Into<String>,
    ) -> Result<(), CoreError> {
        self.storage
            .record_sent_copy(MessageCopy {
                message_id,
                endpoint_id: target_endpoint,
                platform_message_id: platform_message_id.into(),
            })
            .await?;
        Ok(())
    }

    pub async fn acknowledge_dispatch(
        &self,
        acknowledgement: DispatchAcknowledgement,
    ) -> Result<Vec<StoredDispatch>, CoreError> {
        let dispatch = self.storage.dispatch(acknowledgement.dispatch_id).await?;
        if let (Some(thread_id), Some(dispatch)) =
            (acknowledgement.thread_id.clone(), dispatch.as_ref())
        {
            self.persist_provisioned_thread(dispatch, thread_id).await?;
        }
        let mut dispatches = self
            .storage
            .acknowledge_dispatch(
                acknowledgement.dispatch_id,
                acknowledgement.platform_message_id,
            )
            .await?;
        if let Some(dispatch) = dispatch {
            dispatches.extend(self.resume_provisioned(&dispatch).await?);
        }
        Ok(dispatches)
    }

    /// Settle one provisioning dispatch (acknowledged or failed) against its
    /// conversation. Once no provisioning dispatch remains outstanding the
    /// mirror thread is established (or provisioning gave up), so any stashed
    /// messages are replayed and their resulting dispatches returned.
    async fn resume_provisioned(
        &self,
        dispatch: &StoredDispatch,
    ) -> Result<Vec<StoredDispatch>, CoreError> {
        let replica_set_id = dispatch.command.message_replica_set_id();
        let stash = {
            let mut provisioning = self.provisioning.lock().unwrap();
            let Some(state) = provisioning.get_mut(&replica_set_id) else {
                return Ok(Vec::new());
            };
            state.pending.remove(&dispatch.id);
            if !state.pending.is_empty() {
                return Ok(Vec::new());
            }
            provisioning
                .remove(&replica_set_id)
                .map(|state| state.stash)
                .unwrap_or_default()
        };
        let mut dispatches = Vec::new();
        for stashed in stash {
            let (_, routed) = self
                .route_and_record(
                    &stashed.message,
                    stashed.origin_endpoint,
                    &stashed.provision_title,
                )
                .await?;
            dispatches.extend(routed);
        }
        Ok(dispatches)
    }

    /// Record the thread a mirror dispatch created: stamp it onto the endpoint,
    /// register an alias so replies typed into the topic resolve back, and map
    /// the conversation to the thread.
    async fn persist_provisioned_thread(
        &self,
        dispatch: &StoredDispatch,
        thread_id: String,
    ) -> Result<(), CoreError> {
        let connector = dispatch.command.target_connector.clone();
        let container = match &dispatch.command.target.provision_thread {
            Some(provision) => provision.container.clone(),
            None => dispatch.command.target.address.chat_id.clone(),
        };

        let mut endpoint = self
            .storage
            .endpoint(dispatch.command.target_endpoint)
            .await?;
        endpoint.thread_id = Some(thread_id.clone());
        self.storage.put_endpoint(endpoint.clone()).await?;
        self.storage
            .put_alias(Alias {
                connector_id: connector.clone(),
                address: endpoint.address(),
                endpoint_id: endpoint.id,
                is_canonical: true,
            })
            .await?;

        let replica_set_id = dispatch.command.message_replica_set_id();
        if let Some(conversation) = self
            .storage
            .conversation_for_replica_set(replica_set_id)
            .await?
        {
            self.storage
                .put_thread_mapping(wuphf_model::ThreadMapping {
                    conversation_id: conversation.id,
                    connector_id: connector,
                    container,
                    thread_id,
                })
                .await?;
        }
        Ok(())
    }

    /// Handle a platform identity migration: bind the new address to the same
    /// endpoint as the old one and promote it to canonical for outbound sends.
    /// The old alias stays resolvable for inbound. If the old address was never
    /// seen, there is nothing to remap and a later first sighting mints fresh.
    pub async fn migrate_identity(
        &self,
        connector_id: ConnectorId,
        old: PlatformAddress,
        new: PlatformAddress,
    ) -> Result<bool, CoreError> {
        let Some(endpoint_id) = self.storage.resolve_endpoint(&connector_id, &old).await? else {
            return Ok(false);
        };
        self.storage
            .put_alias(Alias {
                connector_id,
                address: new,
                endpoint_id,
                is_canonical: true,
            })
            .await?;
        Ok(true)
    }

    pub async fn pending_dispatches(&self) -> Result<Vec<StoredDispatch>, CoreError> {
        Ok(self.storage.pending_dispatches().await?)
    }

    /// Dead-letter a dispatch the connector reported as permanently undeliverable.
    /// It leaves the pending queue without recording a sent copy, so it is never
    /// redelivered and dependent replies are not released against a phantom send.
    pub async fn fail_dispatch(
        &self,
        dispatch_id: DispatchId,
        reason: String,
    ) -> Result<Vec<StoredDispatch>, CoreError> {
        let dispatch = self.storage.dispatch(dispatch_id).await?;
        self.storage.fail_dispatch(dispatch_id, reason).await?;
        // A failed provisioning dispatch must still release the stash, or the
        // held messages would never send. Replaying them re-attempts the
        // provision with the next message rather than stranding the conversation.
        match dispatch {
            Some(dispatch) => self.resume_provisioned(&dispatch).await,
            None => Ok(Vec::new()),
        }
    }
}

/// The identity-owning side of a rule: the source side if one exists, otherwise
/// the first single side. Used to key the conversation deterministically.
fn identity_side_index(rule: &Rule) -> usize {
    rule.sides
        .iter()
        .position(|side| matches!(side.kind, SideKind::Source { .. }))
        .or_else(|| {
            rule.sides
                .iter()
                .position(|side| matches!(side.kind, SideKind::Single { .. }))
        })
        .expect("a rule has at least one single or source side")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    Routed {
        message_id: MessageId,
        routing: RoutingOutcome,
        dispatches: Vec<StoredDispatch>,
    },
    /// Held because the conversation's mirror thread is still being
    /// provisioned; the message is replayed once the thread exists.
    Deferred {
        message_id: MessageId,
    },
    Ignored(IgnoreReason),
}

impl IngestOutcome {
    pub fn dispatches(&self) -> &[StoredDispatch] {
        match self {
            Self::Routed { dispatches, .. } => dispatches,
            Self::Deferred { .. } | Self::Ignored(_) => &[],
        }
    }
}

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

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Router(#[from] RouterError),
    #[error("origin endpoint {0} resolved to no replica set")]
    OriginWithoutReplicaSet(EndpointId),
    #[error("rule references connector {0} which is not registered")]
    UnknownConnector(ConnectorId),
    #[error("reaction ingestion is not yet implemented")]
    UnsupportedEvent,
    #[error("deleted message {0} maps from a copy but is not stored")]
    DeleteTargetNotStored(MessageId),
    #[error("edited message {0} maps from a copy but is not stored")]
    EditTargetNotStored(MessageId),
    #[error("attachment media {0} is not uploaded to the core")]
    UnknownMedia(MediaId),
    #[error("attachment metadata for media {0} differs from the core record")]
    MediaMetadataMismatch(MediaId),
}

#[cfg(test)]
mod tests;
