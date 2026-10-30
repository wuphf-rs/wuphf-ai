//! Owner messages: deliver an owner-authored text (e.g. typed in the TUI)
//! to a platform chat directly, without an inbound event triggering it.
//!
//! An owner message reuses the ordinary dispatch pipeline end to end, which is what
//! keeps it loop-safe: the direct dispatch is acknowledged like any other, the
//! acknowledgement records a *sent* copy, and when the platform later reports
//! the message back as an inbound event the ingest path recognises the copy and
//! drops it as a [`crate::IgnoreReason::SentMessageEcho`].
//!
//! When the target chat is part of a routing rule the send also fans out to the
//! chat's mirrors, exactly as if the message had arrived from the platform —
//! without this the mirrors would silently miss owner-sent messages, because
//! the platform echo that would normally replicate them is suppressed.

use crate::profile::target_disappearing;
use crate::{Core, CoreError, RoutingConfig};
use std::sync::Arc;
use wuphf_model::{
    Action, Alias, ChatAddress, ChatRef, ConnectorId, Conversation, ConversationId, Dispatch,
    Endpoint, EndpointId, Message, MessageId, OWNER_AUTHOR, OwnerMessage, OwnerSendOutcome,
    ReplyContext, SourceAddress, Target,
};
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    /// Send an owner-authored text to a platform chat. Enqueues the direct
    /// dispatch (and any mirror fan-out) for delivery; the caller forwards the
    /// returned dispatches to the connectors.
    pub async fn send_owner_message(
        &self,
        outbound: OwnerMessage,
    ) -> Result<OwnerSendOutcome, CoreError> {
        self.send_owner_message_with(&self.routing.routing(), outbound)
            .await
    }

    /// [`Self::send_owner_message`] against a routing snapshot the caller
    /// already read, so an operation sending several messages (owner notices)
    /// routes them all against one config.
    pub(crate) async fn send_owner_message_with(
        &self,
        routing: &Arc<RoutingConfig>,
        outbound: OwnerMessage,
    ) -> Result<OwnerSendOutcome, CoreError> {
        let address = ChatAddress::new(outbound.chat_id.clone(), outbound.thread_id.clone());
        let endpoint_id = self
            .owner_message_endpoint(routing, &outbound.connector_id, &address)
            .await?;
        let conversation = self
            .storage
            .conversation_for_endpoint(endpoint_id)
            .await?
            .ok_or(CoreError::OriginWithoutConversation(endpoint_id))?;
        let conversation_id = conversation.id;

        let (reply_to, reply_context) = self
            .resolve_reply(endpoint_id, outbound.reply_to_native_id.as_deref())
            .await?;

        let message = Message {
            id: MessageId::new(),
            conversation_id,
            author_id: OWNER_AUTHOR.to_owned(),
            // Attributed like the owner's own messages from a platform, so a
            // mirror labels it; the direct copy drops the label below.
            participant: Some(OWNER_AUTHOR.to_owned()),
            author_name: Some(OWNER_AUTHOR.to_owned()),
            text: Some(outbound.text),
            attachments: Vec::new(),
            reply_to,
            timestamp_ms: outbound.timestamp_ms,
            deleted: false,
        };
        self.storage.put_message(message.clone()).await?;

        // The owner's own account posts the direct copy natively, so it carries
        // no author label for the connector to render.
        let unlabelled = Message {
            participant: None,
            author_name: None,
            ..message.clone()
        };
        let target = Target {
            endpoint_id,
            connector_id: outbound.connector_id.clone(),
            address,
            create_thread: None,
        };
        let disappearing = target_disappearing(&*self.storage, routing, &target).await?;
        let direct = Dispatch::new(
            target,
            conversation_id,
            Action::Send {
                message: unlabelled,
                reply: reply_context,
                disappearing,
            },
            None,
        );
        self.storage.enqueue_dispatch(&direct).await?;

        let mirrored = self
            .fan_out_to_mirrors(
                routing,
                message,
                endpoint_id,
                conversation,
                &outbound.chat_id,
            )
            .await?;
        Ok(OwnerSendOutcome { direct, mirrored })
    }

    /// The endpoint an owner message originates at. A rule-matched chat resolves (and
    /// lazily mints) through the same path as inbound traffic, so owner messages and
    /// routing agree on the topology; a chat outside every rule gets a
    /// standalone endpoint in its own rule-less conversation, which keeps
    /// later inbound resolution working while fanning out to nobody.
    async fn owner_message_endpoint(
        &self,
        routing: &RoutingConfig,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> Result<EndpointId, CoreError> {
        let source = SourceAddress {
            connector_id: connector_id.clone(),
            address: address.clone(),
            chat_title: None,
        };
        if let Some(endpoint) = self.resolve_origin(routing, &source).await? {
            return Ok(endpoint);
        }
        let info = routing
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
            .put_conversation(Conversation {
                id: ConversationId::new(),
                rule_id: None,
                source: ChatRef {
                    connector_id: connector_id.clone(),
                    address: address.clone(),
                },
                endpoints: vec![endpoint.id],
                title: None,
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

    /// Replicate an owner message to the chat's mirrors through the ordinary routing
    /// fan-out. Honors in-flight mirror-thread provisioning the same way ingest
    /// does: while a provision is pending the message is stashed and replayed
    /// once the thread exists, so an owner message cannot provision a duplicate.
    async fn fan_out_to_mirrors(
        &self,
        routing: &Arc<RoutingConfig>,
        message: Message,
        origin_endpoint: EndpointId,
        conversation: Conversation,
        chat_id: &str,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let conversation_id = conversation.id;
        let provision_title = conversation.title.unwrap_or_else(|| chat_id.to_owned());
        let provisioning = {
            let provisioning = self.provisioning.lock().unwrap();
            provisioning
                .get(&conversation_id)
                .is_some_and(|state| !state.pending.is_empty())
        };
        if provisioning {
            // Stash durably; stash_deferred replays immediately only if the
            // provision settled while we stashed, otherwise the message waits.
            return self
                .stash_deferred(
                    routing,
                    conversation_id,
                    message.id,
                    origin_endpoint,
                    &provision_title,
                )
                .await;
        }
        let (dispatches, _) = self
            .route_and_record(routing, &message, origin_endpoint, &provision_title)
            .await?;
        Ok(dispatches)
    }
}
