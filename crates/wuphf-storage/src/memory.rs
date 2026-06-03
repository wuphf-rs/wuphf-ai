//! In-memory [`Storage`] backend. Keeps all state behind a single `RwLock`,
//! used for tests and ephemeral runs.

use crate::shared::{bind_native_parent, dispatch_state};
use crate::{Storage, StorageError, StorageResult};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;
use wuphf_model::{
    Alias, ChatSettings, ConnectorId, Contact, Conversation, ConversationId, DedupKey,
    DispatchCommand, DispatchId, DispatchState, Endpoint, EndpointId, MediaBlob, MediaId, Message,
    MessageCopy, MessageId, PlatformAddress, ReplicaSet, ReplicaSetId, StoredDispatch,
    ThreadMapping,
};

#[derive(Default)]
pub struct InMemoryStorage {
    state: RwLock<State>,
}

#[derive(Default)]
struct State {
    endpoints: HashMap<EndpointId, Endpoint>,
    replica_sets: HashMap<ReplicaSetId, ReplicaSet>,
    aliases: HashMap<(ConnectorId, PlatformAddress), Alias>,
    conversations: HashMap<ConversationId, Conversation>,
    thread_mappings: HashMap<(ConversationId, ConnectorId), ThreadMapping>,
    messages: HashMap<MessageId, Message>,
    copies: HashMap<(MessageId, EndpointId), MessageCopy>,
    copy_ids: HashMap<(EndpointId, String), MessageId>,
    sent_copies: HashSet<(EndpointId, String)>,
    seen_events: HashSet<DedupKey>,
    contacts: HashMap<String, Contact>,
    chat_settings: HashMap<EndpointId, ChatSettings>,
    media: HashMap<MediaId, StoredMedia>,
    dispatches: HashMap<DispatchId, StoredDispatch>,
}

struct StoredMedia {
    blob: MediaBlob,
    stored_at_ms: i64,
}

#[async_trait]
impl Storage for InMemoryStorage {
    async fn put_endpoint(&self, endpoint: Endpoint) -> StorageResult<()> {
        self.state
            .write()
            .await
            .endpoints
            .insert(endpoint.id, endpoint);
        Ok(())
    }

    async fn endpoint(&self, endpoint_id: EndpointId) -> StorageResult<Endpoint> {
        self.state
            .read()
            .await
            .endpoints
            .get(&endpoint_id)
            .cloned()
            .ok_or(StorageError::EndpointNotFound(endpoint_id))
    }

    async fn put_replica_set(&self, replica_set: ReplicaSet) -> StorageResult<()> {
        self.state
            .write()
            .await
            .replica_sets
            .insert(replica_set.id, replica_set);
        Ok(())
    }

    async fn replica_set(&self, replica_set_id: ReplicaSetId) -> StorageResult<ReplicaSet> {
        self.state
            .read()
            .await
            .replica_sets
            .get(&replica_set_id)
            .cloned()
            .ok_or(StorageError::ReplicaSetNotFound(replica_set_id))
    }

    async fn replica_set_for_endpoint(
        &self,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<ReplicaSetId>> {
        Ok(self
            .state
            .read()
            .await
            .replica_sets
            .values()
            .find(|replica_set| replica_set.endpoints.contains(&endpoint_id))
            .map(|replica_set| replica_set.id))
    }

    async fn put_alias(&self, alias: Alias) -> StorageResult<()> {
        let mut state = self.state.write().await;
        if alias.is_canonical {
            for existing in state.aliases.values_mut() {
                if existing.endpoint_id == alias.endpoint_id {
                    existing.is_canonical = false;
                }
            }
        }
        state
            .aliases
            .insert((alias.connector_id.clone(), alias.address.clone()), alias);
        Ok(())
    }

    async fn resolve_endpoint(
        &self,
        connector_id: &ConnectorId,
        address: &PlatformAddress,
    ) -> StorageResult<Option<EndpointId>> {
        Ok(self
            .state
            .read()
            .await
            .aliases
            .get(&(connector_id.clone(), address.clone()))
            .map(|alias| alias.endpoint_id))
    }

    async fn put_conversation(&self, conversation: Conversation) -> StorageResult<()> {
        self.state
            .write()
            .await
            .conversations
            .insert(conversation.id, conversation);
        Ok(())
    }

    async fn conversation(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<Option<Conversation>> {
        Ok(self
            .state
            .read()
            .await
            .conversations
            .get(&conversation_id)
            .cloned())
    }

    async fn conversation_for_source(
        &self,
        rule_id: &wuphf_model::RuleId,
        connector_id: &ConnectorId,
        address: &PlatformAddress,
    ) -> StorageResult<Option<Conversation>> {
        Ok(self
            .state
            .read()
            .await
            .conversations
            .values()
            .find(|conversation| {
                conversation.rule_id == *rule_id
                    && conversation.source_connector == *connector_id
                    && conversation.source_address == *address
            })
            .cloned())
    }

    async fn conversation_for_replica_set(
        &self,
        replica_set_id: ReplicaSetId,
    ) -> StorageResult<Option<Conversation>> {
        Ok(self
            .state
            .read()
            .await
            .conversations
            .values()
            .find(|conversation| conversation.replica_set_id == replica_set_id)
            .cloned())
    }

    async fn put_thread_mapping(&self, mapping: ThreadMapping) -> StorageResult<()> {
        self.state.write().await.thread_mappings.insert(
            (mapping.conversation_id, mapping.connector_id.clone()),
            mapping,
        );
        Ok(())
    }

    async fn thread_for_conversation(
        &self,
        conversation_id: ConversationId,
        connector_id: &ConnectorId,
    ) -> StorageResult<Option<String>> {
        Ok(self
            .state
            .read()
            .await
            .thread_mappings
            .get(&(conversation_id, connector_id.clone()))
            .map(|mapping| mapping.thread_id.clone()))
    }

    async fn conversation_for_thread(
        &self,
        connector_id: &ConnectorId,
        container: &str,
        thread_id: &str,
    ) -> StorageResult<Option<ConversationId>> {
        Ok(self
            .state
            .read()
            .await
            .thread_mappings
            .values()
            .find(|mapping| {
                mapping.connector_id == *connector_id
                    && mapping.container == container
                    && mapping.thread_id == thread_id
            })
            .map(|mapping| mapping.conversation_id))
    }

    async fn claim_event(&self, key: DedupKey) -> StorageResult<bool> {
        Ok(self.state.write().await.seen_events.insert(key))
    }

    async fn put_message(&self, message: Message) -> StorageResult<()> {
        self.state
            .write()
            .await
            .messages
            .insert(message.id, message);
        Ok(())
    }

    async fn message(&self, message_id: MessageId) -> StorageResult<Option<Message>> {
        Ok(self.state.read().await.messages.get(&message_id).cloned())
    }

    async fn put_received_copy(&self, copy: MessageCopy) -> StorageResult<()> {
        let mut state = self.state.write().await;
        put_copy(&mut state, copy, false);
        Ok(())
    }

    async fn record_sent_copy(&self, copy: MessageCopy) -> StorageResult<()> {
        let mut state = self.state.write().await;
        put_copy(&mut state, copy, true);
        Ok(())
    }

    async fn is_sent_copy(
        &self,
        endpoint_id: EndpointId,
        platform_message_id: &str,
    ) -> StorageResult<bool> {
        Ok(self
            .state
            .read()
            .await
            .sent_copies
            .contains(&(endpoint_id, platform_message_id.to_owned())))
    }

    async fn message_id_for_copy(
        &self,
        endpoint_id: EndpointId,
        platform_message_id: &str,
    ) -> StorageResult<Option<MessageId>> {
        Ok(self
            .state
            .read()
            .await
            .copy_ids
            .get(&(endpoint_id, platform_message_id.to_owned()))
            .copied())
    }

    async fn copy_at_endpoint(
        &self,
        message_id: MessageId,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<MessageCopy>> {
        Ok(self
            .state
            .read()
            .await
            .copies
            .get(&(message_id, endpoint_id))
            .cloned())
    }

    async fn put_contact(&self, contact: Contact) -> StorageResult<()> {
        self.state
            .write()
            .await
            .contacts
            .insert(contact.id.clone(), contact);
        Ok(())
    }

    async fn put_chat_settings(&self, settings: ChatSettings) -> StorageResult<()> {
        self.state
            .write()
            .await
            .chat_settings
            .insert(settings.endpoint_id, settings);
        Ok(())
    }

    async fn put_media(&self, media: MediaBlob, stored_at_ms: i64) -> StorageResult<()> {
        let media_id = media.attachment.media_id;
        self.state.write().await.media.insert(
            media_id,
            StoredMedia {
                blob: media,
                stored_at_ms,
            },
        );
        Ok(())
    }

    async fn media(&self, media_id: MediaId) -> StorageResult<Option<MediaBlob>> {
        Ok(self
            .state
            .read()
            .await
            .media
            .get(&media_id)
            .map(|media| media.blob.clone()))
    }

    async fn prune_unreferenced_media_before(&self, cutoff_ms: i64) -> StorageResult<u64> {
        let mut state = self.state.write().await;
        let referenced = state
            .messages
            .values()
            .flat_map(|message| message.attachments.iter())
            .map(|attachment| attachment.media_id)
            .collect::<HashSet<_>>();
        let before = state.media.len();
        state
            .media
            .retain(|id, media| media.stored_at_ms >= cutoff_ms || referenced.contains(id));
        Ok((before - state.media.len()) as u64)
    }

    async fn enqueue_dispatch(
        &self,
        command: DispatchCommand,
        waiting_for: Option<MessageId>,
    ) -> StorageResult<StoredDispatch> {
        let dispatch = StoredDispatch {
            id: DispatchId::new(),
            command,
            state: dispatch_state(waiting_for),
        };
        self.state
            .write()
            .await
            .dispatches
            .insert(dispatch.id, dispatch.clone());
        Ok(dispatch)
    }

    async fn pending_dispatches(&self) -> StorageResult<Vec<StoredDispatch>> {
        Ok(self
            .state
            .read()
            .await
            .dispatches
            .values()
            .filter(|dispatch| dispatch.state == DispatchState::Pending)
            .cloned()
            .collect())
    }

    async fn dispatch(&self, dispatch_id: DispatchId) -> StorageResult<Option<StoredDispatch>> {
        Ok(self
            .state
            .read()
            .await
            .dispatches
            .get(&dispatch_id)
            .cloned())
    }

    async fn acknowledge_dispatch(
        &self,
        dispatch_id: DispatchId,
        platform_message_id: String,
    ) -> StorageResult<Vec<StoredDispatch>> {
        let mut state = self.state.write().await;
        let dispatch = state
            .dispatches
            .get_mut(&dispatch_id)
            .ok_or(StorageError::DispatchNotFound(dispatch_id))?;
        dispatch.state = DispatchState::Acknowledged {
            platform_message_id: platform_message_id.clone(),
        };
        // A retitle carries no logical message, so it records no copy and
        // releases no replies; acknowledging it just closes out the dispatch.
        let Some(message_id) = dispatch.command.message_id() else {
            return Ok(Vec::new());
        };
        let endpoint_id = dispatch.command.target_endpoint;
        put_copy(
            &mut state,
            MessageCopy {
                message_id,
                endpoint_id,
                platform_message_id: platform_message_id.clone(),
            },
            true,
        );

        let blocked_ids: Vec<DispatchId> = state
            .dispatches
            .values()
            .filter(|candidate| {
                candidate.command.target_endpoint == endpoint_id
                    && candidate.state
                        == (DispatchState::Blocked {
                            waiting_for: message_id,
                        })
            })
            .map(|candidate| candidate.id)
            .collect();
        if blocked_ids.is_empty() {
            return Ok(Vec::new());
        }
        let parent = state
            .messages
            .get(&message_id)
            .cloned()
            .ok_or(StorageError::MessageNotFound(message_id))?;
        let mut released = Vec::new();
        for id in blocked_ids {
            let candidate = state
                .dispatches
                .get_mut(&id)
                .expect("blocked dispatch present");
            bind_native_parent(
                &mut candidate.command,
                parent.clone(),
                platform_message_id.clone(),
            );
            candidate.state = DispatchState::Pending;
            released.push(candidate.clone());
        }
        Ok(released)
    }

    async fn fail_dispatch(&self, dispatch_id: DispatchId, reason: String) -> StorageResult<()> {
        let mut state = self.state.write().await;
        let dispatch = state
            .dispatches
            .get_mut(&dispatch_id)
            .ok_or(StorageError::DispatchNotFound(dispatch_id))?;
        dispatch.state = DispatchState::Failed { reason };
        Ok(())
    }
}

fn put_copy(state: &mut State, copy: MessageCopy, sent: bool) {
    if sent {
        state
            .sent_copies
            .insert((copy.endpoint_id, copy.platform_message_id.clone()));
    }
    state.copy_ids.insert(
        (copy.endpoint_id, copy.platform_message_id.clone()),
        copy.message_id,
    );
    state
        .copies
        .insert((copy.message_id, copy.endpoint_id), copy);
}
