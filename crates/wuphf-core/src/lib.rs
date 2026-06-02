use std::sync::Arc;
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
            EventPayload::MessageDelete { .. } | EventPayload::ReactionAdd(_) => {
                Err(CoreError::UnsupportedEvent)
            }
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

        let reply_to = match inbound.reply_to_platform_message_id.as_deref() {
            Some(platform_message_id) => Some(
                self.storage
                    .message_id_for_copy(origin_endpoint, platform_message_id)
                    .await?
                    .ok_or_else(|| CoreError::UnknownReplyTarget(platform_message_id.to_owned()))?,
            ),
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
        let routing = self
            .router
            .route(&message, origin_endpoint, &provision_title)
            .await?;
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
            message_id: message.id,
            routing,
            dispatches,
        })
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
        if let Some(thread_id) = acknowledgement.thread_id.clone()
            && let Some(dispatch) = self.storage.dispatch(acknowledgement.dispatch_id).await?
        {
            self.persist_provisioned_thread(&dispatch, thread_id)
                .await?;
        }
        Ok(self
            .storage
            .acknowledge_dispatch(
                acknowledgement.dispatch_id,
                acknowledgement.platform_message_id,
            )
            .await?)
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
    ) -> Result<(), CoreError> {
        self.storage.fail_dispatch(dispatch_id, reason).await?;
        Ok(())
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
    Ignored(IgnoreReason),
}

impl IngestOutcome {
    pub fn dispatches(&self) -> &[StoredDispatch] {
        match self {
            Self::Routed { dispatches, .. } => dispatches,
            Self::Ignored(_) => &[],
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
    #[error("message delete and reaction ingestion are not yet implemented")]
    UnsupportedEvent,
    #[error("reply target platform message {0} is unknown at the origin endpoint")]
    UnknownReplyTarget(String),
    #[error("edited message {0} maps from a copy but is not stored")]
    EditTargetNotStored(MessageId),
    #[error("attachment media {0} is not uploaded to the core")]
    UnknownMedia(MediaId),
    #[error("attachment metadata for media {0} differs from the core record")]
    MediaMetadataMismatch(MediaId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use wuphf_model::{
        Attachment, AttachmentKind, ConnectorCapabilities, DispatchPayload, InboundMessage,
        MediaBlob, MediaId, Platform, Rule, RuleId, RuleSide,
    };
    use wuphf_storage::InMemoryStorage;

    const WHATSAPP: &str = "whatsapp-main";
    const TELEGRAM: &str = "telegram-main";

    fn connectors() -> HashMap<ConnectorId, ConnectorInfo> {
        HashMap::from([
            (
                ConnectorId(WHATSAPP.into()),
                ConnectorInfo {
                    platform: Platform::WhatsApp,
                    capabilities: ConnectorCapabilities::default(),
                },
            ),
            (
                ConnectorId(TELEGRAM.into()),
                ConnectorInfo {
                    platform: Platform::Telegram,
                    capabilities: ConnectorCapabilities {
                        supports_threads: true,
                        supports_topic_create: true,
                    },
                },
            ),
        ])
    }

    fn direct_rule() -> Rule {
        Rule {
            id: RuleId("direct".into()),
            sides: vec![
                RuleSide {
                    connector: ConnectorId(WHATSAPP.into()),
                    kind: SideKind::Single {
                        chat_id: "12345@s.whatsapp.net".into(),
                        thread_id: None,
                    },
                },
                RuleSide {
                    connector: ConnectorId(TELEGRAM.into()),
                    kind: SideKind::Single {
                        chat_id: "-100123".into(),
                        thread_id: Some("12".into()),
                    },
                },
            ],
        }
    }

    fn mirror_rule() -> Rule {
        Rule {
            id: RuleId("wa-mirror".into()),
            sides: vec![
                RuleSide {
                    connector: ConnectorId(WHATSAPP.into()),
                    kind: SideKind::Source {
                        include: vec!["111@g.us".into()],
                        exclude: vec![],
                    },
                },
                RuleSide {
                    connector: ConnectorId(TELEGRAM.into()),
                    kind: SideKind::Mirror {
                        container: "-1001234567".into(),
                    },
                },
            ],
        }
    }

    fn core(rules: Vec<Rule>) -> Core<InMemoryStorage> {
        let storage = Arc::new(InMemoryStorage::default());
        let config = Arc::new(RoutingConfig::new(rules, connectors()));
        Core::new(storage, config)
    }

    fn event(
        connector: &str,
        chat_id: &str,
        native_id: &str,
        reply_to: Option<&str>,
    ) -> NormalizedEvent {
        event_in(connector, chat_id, None, native_id, reply_to)
    }

    fn event_in(
        connector: &str,
        chat_id: &str,
        thread_id: Option<&str>,
        native_id: &str,
        reply_to: Option<&str>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            source: SourceAddress {
                connector_id: ConnectorId(connector.into()),
                address: PlatformAddress::new(chat_id, thread_id.map(str::to_owned)),
                chat_title: Some("Title".into()),
            },
            payload: EventPayload::MessageCreate(InboundMessage {
                platform_message_id: native_id.into(),
                sender: "alice".into(),
                participant: None,
                sender_name: None,
                text: Some("hello".into()),
                attachments: vec![],
                reply_to_platform_message_id: reply_to.map(str::to_owned),
                timestamp_ms: 1,
            }),
        }
    }

    #[tokio::test]
    async fn direct_rule_mints_endpoints_and_fans_out_to_the_paired_single() {
        let core = core(vec![direct_rule()]);
        let outcome = core
            .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
            .await
            .unwrap();
        let dispatches = outcome.dispatches();
        assert_eq!(dispatches.len(), 1);
        let command = &dispatches[0].command;
        assert_eq!(command.target_connector, ConnectorId(TELEGRAM.into()));
        assert_eq!(
            command.target.address,
            PlatformAddress::new("-100123", Some("12".into()))
        );
        assert!(command.target.provision_thread.is_none());
    }

    #[tokio::test]
    async fn unmatched_address_is_ignored() {
        let core = core(vec![direct_rule()]);
        let outcome = core
            .ingest(event(WHATSAPP, "99999@s.whatsapp.net", "wa-1", None))
            .await
            .unwrap();
        assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
    }

    #[tokio::test]
    async fn source_excluded_by_filter_is_ignored() {
        let core = core(vec![mirror_rule()]);
        let outcome = core
            .ingest(event(WHATSAPP, "222@g.us", "wa-1", None))
            .await
            .unwrap();
        assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
    }

    #[tokio::test]
    async fn mirror_dispatch_requests_thread_provisioning_then_persists_mapping() {
        let core = core(vec![mirror_rule()]);
        let outcome = core
            .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
            .await
            .unwrap();
        let dispatch = &outcome.dispatches()[0];
        let provision = dispatch
            .command
            .target
            .provision_thread
            .as_ref()
            .expect("first mirror send provisions a thread");
        assert_eq!(provision.container, "-1001234567");
        assert_eq!(provision.title, "Title");

        core.acknowledge_dispatch(DispatchAcknowledgement {
            dispatch_id: dispatch.id,
            platform_message_id: "tg-1".into(),
            thread_id: Some("777".into()),
        })
        .await
        .unwrap();

        // A reply typed into the provisioned topic resolves back to the source.
        let reverse = core
            .ingest(event_in(
                TELEGRAM,
                "-1001234567",
                Some("777"),
                "tg-reply",
                None,
            ))
            .await
            .unwrap();
        let back = &reverse.dispatches()[0];
        assert_eq!(back.command.target_connector, ConnectorId(WHATSAPP.into()));
        assert_eq!(back.command.target.address.chat_id, "111@g.us");

        // The next source message reuses the thread without provisioning.
        let again = core
            .ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
            .await
            .unwrap();
        let resend = &again.dispatches()[0];
        assert_eq!(
            resend.command.target.address,
            PlatformAddress::new("-1001234567", Some("777".into()))
        );
        assert!(resend.command.target.provision_thread.is_none());
    }

    #[tokio::test]
    async fn ignores_echoes_of_dispatched_copies() {
        let core = core(vec![direct_rule()]);
        let outcome = core
            .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
            .await
            .unwrap();
        let IngestOutcome::Routed {
            message_id,
            dispatches,
            ..
        } = outcome
        else {
            panic!("message should route");
        };
        let telegram = dispatches[0].command.target_endpoint;
        core.record_dispatched_copy(message_id, telegram, "tg-9")
            .await
            .unwrap();
        assert_eq!(
            core.ingest(event_in(TELEGRAM, "-100123", Some("12"), "tg-9", None))
                .await
                .unwrap(),
            IngestOutcome::Ignored(IgnoreReason::SentMessageEcho)
        );
    }

    #[tokio::test]
    async fn ignores_duplicate_events() {
        let core = core(vec![direct_rule()]);
        let event = event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None);
        core.ingest(event.clone()).await.unwrap();
        assert_eq!(
            core.ingest(event).await.unwrap(),
            IngestOutcome::Ignored(IgnoreReason::DuplicateEvent)
        );
    }

    #[tokio::test]
    async fn maps_replies_through_internal_identity_and_target_copy() {
        let core = core(vec![direct_rule()]);
        let parent = core
            .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-parent", None))
            .await
            .unwrap();
        let IngestOutcome::Routed {
            message_id: parent_id,
            dispatches,
            ..
        } = parent
        else {
            panic!("parent should route");
        };
        let telegram = dispatches[0].command.target_endpoint;
        core.record_dispatched_copy(parent_id, telegram, "tg-parent")
            .await
            .unwrap();

        let reply = core
            .ingest(event(
                WHATSAPP,
                "12345@s.whatsapp.net",
                "wa-reply",
                Some("wa-parent"),
            ))
            .await
            .unwrap();
        let DispatchPayload::MessageCreate {
            message,
            reply_context,
        } = &reply.dispatches()[0].command.payload
        else {
            panic!("reply should route as a message create");
        };
        assert_eq!(message.reply_to, Some(parent_id));
        let reply_context = reply_context.as_ref().expect("reply context resolved");
        assert_eq!(reply_context.message_id, parent_id);
        assert_eq!(reply_context.platform_message_id, "tg-parent");
    }

    #[tokio::test]
    async fn blocks_reply_dispatch_until_target_parent_copy_exists() {
        let core = core(vec![direct_rule()]);
        core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-parent", None))
            .await
            .unwrap();
        let reply = core
            .ingest(event(
                WHATSAPP,
                "12345@s.whatsapp.net",
                "wa-reply",
                Some("wa-parent"),
            ))
            .await
            .unwrap();
        let IngestOutcome::Routed { routing, .. } = reply else {
            panic!("reply should be stored while delivery is blocked");
        };
        assert!(routing.dispatches.is_empty());
        assert_eq!(routing.blocked.len(), 1);
    }

    #[tokio::test]
    async fn migrated_address_resolves_to_the_same_endpoint() {
        let core = core(vec![mirror_rule()]);
        let first = core
            .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
            .await
            .unwrap();
        let IngestOutcome::Routed { message_id, .. } = first else {
            panic!("first message should route");
        };

        assert!(
            core.migrate_identity(
                ConnectorId(WHATSAPP.into()),
                PlatformAddress::new("111@g.us", None),
                PlatformAddress::new("111@lid", None),
            )
            .await
            .unwrap()
        );

        // The new address routes into the same conversation (no new mint), so a
        // message under it fans out to the already-provisioned mirror endpoint.
        let migrated = core
            .ingest(event(WHATSAPP, "111@lid", "wa-2", None))
            .await
            .unwrap();
        let IngestOutcome::Routed {
            message_id: migrated_id,
            dispatches,
            ..
        } = migrated
        else {
            panic!("migrated message should route");
        };
        assert_ne!(message_id, migrated_id);
        assert_eq!(dispatches.len(), 1);
        assert_eq!(
            dispatches[0].command.target_connector,
            ConnectorId(TELEGRAM.into())
        );
    }

    #[tokio::test]
    async fn rejects_unknown_or_modified_media_before_claiming_event() {
        let core = core(vec![direct_rule()]);
        let attachment = Attachment {
            media_id: MediaId::new(),
            kind: AttachmentKind::Image,
            file_name: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            size_bytes: Some(5),
        };
        let media_event = |attachment: Attachment| {
            let mut event = event(WHATSAPP, "12345@s.whatsapp.net", "wa-media", None);
            let EventPayload::MessageCreate(inbound) = &mut event.payload else {
                unreachable!();
            };
            inbound.attachments = vec![attachment];
            event
        };
        assert!(matches!(
            core.ingest(media_event(attachment.clone())).await,
            Err(CoreError::UnknownMedia(id)) if id == attachment.media_id
        ));

        core.storage
            .put_media(
                MediaBlob {
                    attachment: attachment.clone(),
                    bytes: b"hello".to_vec(),
                },
                1,
            )
            .await
            .unwrap();
        let mut altered = attachment.clone();
        altered.kind = AttachmentKind::File;
        assert!(matches!(
            core.ingest(media_event(altered)).await,
            Err(CoreError::MediaMetadataMismatch(id)) if id == attachment.media_id
        ));

        assert_eq!(
            core.ingest(media_event(attachment))
                .await
                .unwrap()
                .dispatches()
                .len(),
            1
        );
    }

    fn edit_event(
        connector: &str,
        chat_id: &str,
        thread_id: Option<&str>,
        native_id: &str,
        text: Option<&str>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            source: SourceAddress {
                connector_id: ConnectorId(connector.into()),
                address: PlatformAddress::new(chat_id, thread_id.map(str::to_owned)),
                chat_title: None,
            },
            payload: EventPayload::MessageEdit {
                platform_message_id: native_id.into(),
                text: text.map(str::to_owned),
            },
        }
    }

    #[tokio::test]
    async fn edit_after_delivery_revises_the_delivered_copy() {
        let core = core(vec![direct_rule()]);
        let IngestOutcome::Routed {
            message_id,
            dispatches,
            ..
        } = core
            .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
            .await
            .unwrap()
        else {
            panic!("create should route");
        };
        let telegram = dispatches[0].command.target_endpoint;
        core.record_dispatched_copy(message_id, telegram, "tg-1")
            .await
            .unwrap();

        let edited = core
            .ingest(edit_event(
                WHATSAPP,
                "12345@s.whatsapp.net",
                None,
                "wa-1",
                Some("hello edited"),
            ))
            .await
            .unwrap();
        let dispatch = &edited.dispatches()[0];
        assert_eq!(
            dispatch.command.target_connector,
            ConnectorId(TELEGRAM.into())
        );
        let DispatchPayload::MessageEdit {
            message,
            target_platform_message_id,
        } = &dispatch.command.payload
        else {
            panic!("edit should route as a message edit");
        };
        assert_eq!(target_platform_message_id, "tg-1");
        assert_eq!(message.text.as_deref(), Some("hello edited"));
    }

    #[tokio::test]
    async fn edit_before_delivery_blocks_then_releases_with_native_id() {
        let core = core(vec![direct_rule()]);
        let IngestOutcome::Routed { dispatches, .. } = core
            .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
            .await
            .unwrap()
        else {
            panic!("create should route");
        };
        let create_dispatch = dispatches[0].clone();

        let edited = core
            .ingest(edit_event(
                WHATSAPP,
                "12345@s.whatsapp.net",
                None,
                "wa-1",
                Some("edited before delivery"),
            ))
            .await
            .unwrap();
        let IngestOutcome::Routed {
            routing,
            dispatches,
            ..
        } = edited
        else {
            panic!("edit should be stored while delivery is blocked");
        };
        assert!(dispatches.is_empty());
        assert_eq!(routing.blocked.len(), 1);

        let released = core
            .acknowledge_dispatch(DispatchAcknowledgement {
                dispatch_id: create_dispatch.id,
                platform_message_id: "tg-1".into(),
                thread_id: None,
            })
            .await
            .unwrap();
        let edit = released
            .iter()
            .find(|dispatch| {
                matches!(
                    dispatch.command.payload,
                    DispatchPayload::MessageEdit { .. }
                )
            })
            .expect("acknowledging the create releases the blocked edit");
        let DispatchPayload::MessageEdit {
            target_platform_message_id,
            ..
        } = &edit.command.payload
        else {
            unreachable!();
        };
        assert_eq!(target_platform_message_id, "tg-1");
    }

    #[tokio::test]
    async fn edit_that_does_not_change_text_is_ignored() {
        let core = core(vec![direct_rule()]);
        core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
            .await
            .unwrap();
        assert_eq!(
            core.ingest(edit_event(
                WHATSAPP,
                "12345@s.whatsapp.net",
                None,
                "wa-1",
                Some("hello"),
            ))
            .await
            .unwrap(),
            IngestOutcome::Ignored(IgnoreReason::RedundantEdit)
        );
    }

    #[tokio::test]
    async fn edit_for_unknown_message_is_ignored() {
        let core = core(vec![direct_rule()]);
        assert_eq!(
            core.ingest(edit_event(
                WHATSAPP,
                "12345@s.whatsapp.net",
                None,
                "wa-unknown",
                Some("edited"),
            ))
            .await
            .unwrap(),
            IngestOutcome::Ignored(IgnoreReason::Unmatched)
        );
    }
}
