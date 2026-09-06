//! Raw outbound sending: deliver an owner-authored text (e.g. typed in the TUI)
//! to a platform chat directly, without an inbound event triggering it.
//!
//! A raw send reuses the ordinary dispatch pipeline end to end, which is what
//! keeps it loop-safe: the direct dispatch is acknowledged like any other, the
//! acknowledgement records a *sent* copy, and when the platform later reports
//! the message back as an inbound event the ingest path recognises the copy and
//! drops it as a [`crate::IgnoreReason::SentMessageEcho`].
//!
//! When the target chat is part of a routing rule the send also fans out to the
//! chat's mirrors, exactly as if the message had arrived from the platform —
//! without this the mirrors would silently miss owner-sent messages, because
//! the platform echo that would normally replicate them is suppressed.

use crate::{Core, CoreError};
use wuphf_model::{
    Alias, ConnectorId, DispatchCommand, DispatchPayload, Endpoint, EndpointId, Message, MessageId,
    PlatformAddress, RAW_SENDER, RawOutbound, RawSendOutcome, ReplicaSet, ReplicaSetId,
    ReplyContext, SourceAddress, StoredDispatch, TargetAddress,
};
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    /// Send an owner-authored text to a platform chat. Enqueues the direct
    /// dispatch (and any mirror fan-out) for delivery; the caller forwards the
    /// returned dispatches to the connectors.
    pub async fn send_raw(&self, outbound: RawOutbound) -> Result<RawSendOutcome, CoreError> {
        let address = PlatformAddress::new(outbound.chat_id.clone(), outbound.thread_id.clone());
        let endpoint_id = self
            .raw_target_endpoint(&outbound.connector_id, &address)
            .await?;
        let replica_set_id = self
            .storage
            .replica_set_for_endpoint(endpoint_id)
            .await?
            .ok_or(CoreError::OriginWithoutReplicaSet(endpoint_id))?;

        let (reply_to, reply_context) = self
            .resolve_reply(
                endpoint_id,
                outbound.reply_to_platform_message_id.as_deref(),
            )
            .await?;

        let message = Message {
            id: MessageId::new(),
            replica_set_id,
            sender: RAW_SENDER.to_owned(),
            participant: None,
            sender_name: None,
            text: Some(outbound.text),
            attachments: Vec::new(),
            reply_to,
            timestamp_ms: outbound.timestamp_ms,
            deleted: false,
        };
        self.storage.put_message(message.clone()).await?;

        let direct = self
            .storage
            .enqueue_dispatch(
                DispatchCommand {
                    target_endpoint: endpoint_id,
                    target_connector: outbound.connector_id.clone(),
                    target: TargetAddress {
                        address,
                        provision_thread: None,
                    },
                    payload: DispatchPayload::MessageCreate {
                        message: message.clone(),
                        reply_context,
                    },
                },
                None,
            )
            .await?;

        let mirrored = self
            .fan_out_to_mirrors(message, endpoint_id, replica_set_id, &outbound.chat_id)
            .await?;
        Ok(RawSendOutcome { direct, mirrored })
    }

    /// The endpoint a raw send originates at. A rule-matched chat resolves (and
    /// lazily mints) through the same path as inbound traffic, so raw sends and
    /// routing agree on the topology; a chat outside every rule gets a
    /// standalone endpoint in its own single-member replica set, which keeps
    /// later inbound resolution working while fanning out to nobody.
    async fn raw_target_endpoint(
        &self,
        connector_id: &ConnectorId,
        address: &PlatformAddress,
    ) -> Result<EndpointId, CoreError> {
        let source = SourceAddress {
            connector_id: connector_id.clone(),
            address: address.clone(),
            chat_title: None,
        };
        if let Some(endpoint) = self.resolve_origin(&source).await? {
            return Ok(endpoint);
        }
        let info = self
            .config
            .connector(connector_id)
            .ok_or_else(|| CoreError::UnknownConnector(connector_id.clone()))?;
        let endpoint = Endpoint {
            id: EndpointId::new(),
            connector_id: connector_id.clone(),
            platform: info.platform.clone(),
            chat_id: address.chat_id.clone(),
            thread_id: address.thread_id.clone(),
        };
        self.storage.put_endpoint(endpoint.clone()).await?;
        self.storage
            .put_alias(Alias {
                connector_id: connector_id.clone(),
                address: address.clone(),
                endpoint_id: endpoint.id,
                is_canonical: true,
            })
            .await?;
        self.storage
            .put_replica_set(ReplicaSet {
                id: ReplicaSetId::new(),
                endpoints: vec![endpoint.id],
            })
            .await?;
        Ok(endpoint.id)
    }

    /// Resolve the reply parent best-effort: when the quoted native id maps to a
    /// stored message at this endpoint the reply links and quotes it; otherwise
    /// the send goes out as a plain message.
    async fn resolve_reply(
        &self,
        endpoint_id: EndpointId,
        reply_to_platform_message_id: Option<&str>,
    ) -> Result<(Option<MessageId>, Option<ReplyContext>), CoreError> {
        let Some(platform_message_id) = reply_to_platform_message_id else {
            return Ok((None, None));
        };
        let Some(parent_id) = self
            .storage
            .message_id_for_copy(endpoint_id, platform_message_id)
            .await?
        else {
            return Ok((None, None));
        };
        let Some(parent) = self.storage.message(parent_id).await? else {
            return Ok((None, None));
        };
        let authored_by_bridge = self
            .storage
            .is_sent_copy(endpoint_id, platform_message_id)
            .await?;
        let context =
            ReplyContext::quoting(parent, platform_message_id.to_owned(), authored_by_bridge);
        Ok((Some(parent_id), Some(context)))
    }

    /// Replicate a raw send to the chat's mirrors through the ordinary routing
    /// fan-out. Honors in-flight mirror-thread provisioning the same way ingest
    /// does: while a provision is pending the message is stashed and replayed
    /// once the thread exists, so a raw send cannot provision a duplicate.
    async fn fan_out_to_mirrors(
        &self,
        message: Message,
        origin_endpoint: EndpointId,
        replica_set_id: ReplicaSetId,
        chat_id: &str,
    ) -> Result<Vec<StoredDispatch>, CoreError> {
        let provision_title = match self
            .storage
            .conversation_for_replica_set(replica_set_id)
            .await?
        {
            Some(conversation) => conversation.title.unwrap_or_else(|| chat_id.to_owned()),
            None => chat_id.to_owned(),
        };
        let provisioning = {
            let provisioning = self.provisioning.lock().unwrap();
            provisioning
                .get(&replica_set_id)
                .is_some_and(|state| !state.pending.is_empty())
        };
        if provisioning {
            // Stash durably; stash_deferred replays immediately only if the
            // provision settled while we stashed, otherwise the message waits.
            return self
                .stash_deferred(
                    replica_set_id,
                    message.id,
                    origin_endpoint,
                    &provision_title,
                )
                .await;
        }
        let (_, dispatches) = self
            .route_and_record(&message, origin_endpoint, &provision_title)
            .await?;
        Ok(dispatches)
    }
}
