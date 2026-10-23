//! Inbound event handling: turn a normalized create/edit/delete into a stored
//! logical message and the dispatches that fan it out, including mirror-thread
//! provisioning and source-rename retitling.

use crate::{Core, CoreError};
use wuphf_model::{
    DedupKey, Endpoint, EndpointId, IgnoreReason, IngestOutcome, Message, MessageCopy, MessageId,
    ReplicaSetId, RoutingOutcome, SourceAddress, Dispatch,
};
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    pub(crate) async fn ingest_create(
        &self,
        source: SourceAddress,
        origin_endpoint: EndpointId,
        origin: Endpoint,
        replica_set_id: ReplicaSetId,
        inbound: wuphf_model::NewMessage,
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

        // The native id outlives the inbound form: the message drops it on
        // promotion, but the received copy is keyed by it.
        let platform_message_id = inbound.platform_message_id.clone();
        let message = Message::from_inbound(inbound, replica_set_id, reply_to);
        self.storage.put_message(message.clone()).await?;
        self.storage
            .put_received_copy(MessageCopy {
                message_id: message.id,
                endpoint_id: origin_endpoint,
                platform_message_id,
            })
            .await?;

        let provision_title = source
            .chat_title
            .clone()
            .unwrap_or_else(|| source.address.chat_id.clone());

        // Hold the message if this conversation's mirror thread is still being
        // provisioned; it is stashed durably and replayed once the thread exists.
        // This prevents a burst of messages from each provisioning a duplicate
        // thread, and — unlike an in-memory stash — survives a crash.
        let provisioning = {
            let provisioning = self.provisioning.lock().unwrap();
            provisioning
                .get(&replica_set_id)
                .is_some_and(|state| !state.pending.is_empty())
        };
        if provisioning {
            let replayed = self
                .stash_deferred(
                    replica_set_id,
                    message.id,
                    origin_endpoint,
                    &provision_title,
                )
                .await?;
            // The provision is normally still pending, so the message waits
            // (Deferred). If it settled while we stashed, stash_deferred replayed
            // the stash instead, and those dispatches carry this message out.
            if replayed.is_empty() {
                return Ok(IngestOutcome::Deferred {
                    message_id: message.id,
                });
            }
            return Ok(IngestOutcome::Routed {
                message_id: message.id,
                routing: RoutingOutcome::default(),
                dispatches: replayed,
            });
        }

        let (routing, mut dispatches) = self
            .route_and_record(&message, origin_endpoint, &provision_title)
            .await?;
        dispatches.extend(
            self.maybe_retitle(&origin, replica_set_id, source.chat_title.as_deref())
                .await?,
        );
        Ok(IngestOutcome::Routed {
            message_id: message.id,
            routing,
            dispatches,
        })
    }

    /// Detect a source chat title change and fan it out to the mirror threads.
    /// The conversation title is tracked only from its source side; a message
    /// from any other endpoint (e.g. a reply typed into a mirror topic) carries
    /// its own container's title and must not retitle the conversation.
    ///
    /// A newly recorded title only dispatches when a mirror thread already
    /// exists: on the message that first provisions a thread the mapping is not
    /// yet acknowledged, so `route_retitle` naturally yields nothing and the
    /// thread simply takes the provisioned title. When a thread already exists —
    /// including one provisioned before any title was known (named with the raw
    /// chat id) — the first real title is a genuine change and is fanned out.
    async fn maybe_retitle(
        &self,
        origin: &Endpoint,
        replica_set_id: ReplicaSetId,
        new_title: Option<&str>,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let Some(new_title) = new_title.map(str::trim).filter(|title| !title.is_empty()) else {
            return Ok(Vec::new());
        };
        let Some(mut conversation) = self
            .storage
            .conversation_for_replica_set(replica_set_id)
            .await?
        else {
            return Ok(Vec::new());
        };
        if conversation.source_connector != origin.connector_id
            || conversation.source_address.chat_id != origin.chat_id
        {
            return Ok(Vec::new());
        }
        if conversation.title.as_deref() == Some(new_title) {
            return Ok(Vec::new());
        }
        conversation.title = Some(new_title.to_owned());
        self.storage.put_conversation(conversation.clone()).await?;
        let routing = self.router.route_retitle(&conversation, new_title).await?;
        self.enqueue_outcome(&routing).await
    }

    /// Resolve the copy an inbound edit/delete/reaction/receipt targets to its
    /// logical message. `Ok(None)` means the copy is unknown (the caller drops
    /// the event as `Unmatched`); a copy that maps to a message that is somehow
    /// no longer stored is the per-operation `missing` error.
    async fn message_for_copy(
        &self,
        endpoint: EndpointId,
        platform_message_id: &str,
        missing: fn(MessageId) -> CoreError,
    ) -> Result<Option<(MessageId, Message)>, CoreError> {
        let Some(message_id) = self
            .storage
            .message_id_for_copy(endpoint, platform_message_id)
            .await?
        else {
            return Ok(None);
        };
        let message = self
            .storage
            .message(message_id)
            .await?
            .ok_or_else(|| missing(message_id))?;
        Ok(Some((message_id, message)))
    }

    /// Persist a routing outcome: enqueue the sendable dispatches and park the
    /// blocked ones on the message they wait for. Returns only the sendable
    /// dispatches — blocked ones surface later, when their parent is acknowledged.
    async fn enqueue_outcome(
        &self,
        routing: &RoutingOutcome,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let mut dispatches = Vec::new();
        for command in routing.dispatches.iter().cloned() {
            dispatches.push(self.storage.enqueue_dispatch(command, None).await?);
        }
        for blocked in routing.blocked.iter().cloned() {
            self.storage
                .enqueue_dispatch(blocked.command, Some(blocked.waiting_for_message))
                .await?;
        }
        Ok(dispatches)
    }

    /// Route a message, enqueue its dispatches, and register any thread
    /// provisioning it triggers so that concurrent messages to the same
    /// conversation are stashed (rather than provisioning again) until the
    /// thread exists. Returns the routing and the immediately-sendable
    /// dispatches.
    pub(crate) async fn route_and_record(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> Result<(RoutingOutcome, Vec<Dispatch>), CoreError> {
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
    pub(crate) async fn ingest_edit(
        &self,
        origin_endpoint: EndpointId,
        platform_message_id: String,
        text: Option<String>,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, mut message)) = self
            .message_for_copy(
                origin_endpoint,
                &platform_message_id,
                CoreError::EditTargetNotStored,
            )
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        if message.text == text {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantEdit));
        }
        message.text = text;
        self.storage.put_message(message.clone()).await?;

        let routing = self.router.route_edit(&message, origin_endpoint).await?;
        let dispatches = self.enqueue_outcome(&routing).await?;
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
    pub(crate) async fn ingest_delete(
        &self,
        origin_endpoint: EndpointId,
        platform_message_id: String,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, mut message)) = self
            .message_for_copy(
                origin_endpoint,
                &platform_message_id,
                CoreError::DeleteTargetNotStored,
            )
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        if message.deleted {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantDelete));
        }
        message.deleted = true;
        self.storage.put_message(message.clone()).await?;

        let routing = self.router.route_delete(&message, origin_endpoint).await?;
        let dispatches = self.enqueue_outcome(&routing).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            routing,
            dispatches,
        })
    }

    /// Apply an inbound reaction: resolve the reacted copy to its logical
    /// message and fan the reaction out to the other endpoints, blocking targets
    /// whose copy is not delivered yet. The reaction is a toggle (`emoji` None
    /// clears the reactor's reaction); the connector decides how to render it.
    /// No echo guard is needed in the source-to-text direction, as the bridge
    /// never applies a native reaction the source could report back.
    pub(crate) async fn ingest_reaction(
        &self,
        origin_endpoint: EndpointId,
        platform_message_id: String,
        reactor: String,
        reactor_name: Option<String>,
        emoji: Option<String>,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, message)) = self
            .message_for_copy(
                origin_endpoint,
                &platform_message_id,
                CoreError::ReactionTargetNotStored,
            )
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };

        let routing = self
            .router
            .route_reaction(&message, origin_endpoint, reactor, reactor_name, emoji)
            .await?;
        let dispatches = self.enqueue_outcome(&routing).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            routing,
            dispatches,
        })
    }

    /// Surface a delivery/read receipt on the message's other copies. The
    /// receipt is observed at the endpoint where the peer acknowledged it; the
    /// signal fans to every other endpoint that holds a copy (a 👍 for
    /// delivered, 👀 for read), so the side the owner reads the mirror on shows
    /// the message landed/was seen. Because it keys on the logical message and
    /// not on which side authored it, this works both for messages the bridge
    /// sent and for messages the owner sent from another of the source's devices
    /// — the latter arrive mirrored to the other endpoints, where the receipt is
    /// now surfaced. The owner's own (from_me) receipts are ignored. Targets
    /// whose copy is not delivered yet are blocked until it is. The connector
    /// decides how to render the signal.
    pub(crate) async fn ingest_receipt(
        &self,
        observer_endpoint: EndpointId,
        platform_message_id: String,
        from_me: bool,
        kind: wuphf_model::ReceiptKind,
    ) -> Result<IngestOutcome, CoreError> {
        if from_me {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        }
        let Some((message_id, message)) = self
            .message_for_copy(
                observer_endpoint,
                &platform_message_id,
                CoreError::ReceiptTargetNotStored,
            )
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };

        let routing = self
            .router
            .route_receipt(&message, observer_endpoint, kind)
            .await?;
        let dispatches = self.enqueue_outcome(&routing).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            routing,
            dispatches,
        })
    }
}
