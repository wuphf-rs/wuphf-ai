//! In-memory [`Storage`] backend. Keeps all state behind a single `RwLock`,
//! used for tests and ephemeral runs.

use crate::shared::{bind_native_parent, dispatch_state};
use crate::{Storage, StorageError, StorageResult};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;
use wuphf_model::{
    AddressLink, Alias, ChatSettings, ConnectorId, Contact, Conversation, ConversationId, DedupKey,
    DispatchCommand, DispatchId, DispatchState, Endpoint, EndpointId, MediaBlob, MediaId, Message,
    MessageCopy, MessageId, ObservedChat, ObservedDirection, ObservedMessage, PlatformAddress,
    ReplicaSet, ReplicaSetId, StoredDispatch, ThreadMapping,
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
    /// Symmetric address equivalences, mirroring the SQLite `address_links`
    /// table. Kept apart from `aliases` because a link exists with or without an
    /// endpoint behind either address.
    address_links: HashMap<(ConnectorId, PlatformAddress), Vec<PlatformAddress>>,
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
    /// Provisioning stash, keyed by replica set, insertion-ordered. Mirrors the
    /// SQLite `provisioning_stash` table; the message body is resolved from
    /// `messages` on drain.
    provisioning_stash: HashMap<ReplicaSetId, Vec<(MessageId, EndpointId, String)>>,
    observed_messages: HashMap<(ConnectorId, String, String), ObservedMessage>,
    observed_chats: HashMap<(ConnectorId, String), ObservedChat>,
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

    async fn put_address_link(&self, link: AddressLink) -> StorageResult<()> {
        if link.address == link.linked_address {
            return Ok(());
        }
        let mut state = self.state.write().await;
        for (from, to) in [
            (link.address.clone(), link.linked_address.clone()),
            (link.linked_address, link.address),
        ] {
            let linked = state
                .address_links
                .entry((link.connector_id.clone(), from))
                .or_default();
            if !linked.contains(&to) {
                linked.push(to);
            }
        }
        Ok(())
    }

    async fn linked_addresses(
        &self,
        connector_id: &ConnectorId,
        address: &PlatformAddress,
    ) -> StorageResult<Vec<PlatformAddress>> {
        Ok(self
            .state
            .read()
            .await
            .address_links
            .get(&(connector_id.clone(), address.clone()))
            .cloned()
            .unwrap_or_default())
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

    async fn received_copy(&self, message_id: MessageId) -> StorageResult<Option<MessageCopy>> {
        let state = self.state.read().await;
        Ok(state
            .copies
            .iter()
            .find(|((copy_message_id, _), copy)| {
                *copy_message_id == message_id
                    && !state
                        .sent_copies
                        .contains(&(copy.endpoint_id, copy.platform_message_id.clone()))
            })
            .map(|(_, copy)| copy.clone()))
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
        // Signals that act on an existing copy (reactions, delivery acks, seen
        // receipts) and retitles record no copy and release no replies;
        // acknowledging them just closes out the dispatch.
        if !dispatch.command.records_copy() {
            return Ok(Vec::new());
        }
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

    async fn stash_provisioning_message(
        &self,
        replica_set_id: ReplicaSetId,
        message_id: MessageId,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> StorageResult<()> {
        let mut state = self.state.write().await;
        let entries = state.provisioning_stash.entry(replica_set_id).or_default();
        // Idempotent on (replica_set_id, message_id), matching the SQLite PK.
        if entries.iter().any(|(id, _, _)| *id == message_id) {
            return Ok(());
        }
        entries.push((message_id, origin_endpoint, provision_title.to_owned()));
        Ok(())
    }

    async fn drain_provisioning_stash(
        &self,
        replica_set_id: ReplicaSetId,
    ) -> StorageResult<Vec<(Message, EndpointId, String)>> {
        let mut state = self.state.write().await;
        let Some(entries) = state.provisioning_stash.remove(&replica_set_id) else {
            return Ok(Vec::new());
        };
        // Resolve each row back to its message body, dropping any whose message
        // is no longer stored (mirrors the SQLite inner join).
        let drained = entries
            .into_iter()
            .filter_map(|(message_id, origin_endpoint, provision_title)| {
                state
                    .messages
                    .get(&message_id)
                    .cloned()
                    .map(|message| (message, origin_endpoint, provision_title))
            })
            .collect();
        Ok(drained)
    }

    async fn stashed_replica_sets(&self) -> StorageResult<Vec<ReplicaSetId>> {
        Ok(self
            .state
            .read()
            .await
            .provisioning_stash
            .keys()
            .copied()
            .collect())
    }

    async fn log_observed(
        &self,
        mut observed: ObservedMessage,
        chat_title: Option<String>,
    ) -> StorageResult<ObservedMessage> {
        let mut state = self.state.write().await;
        let message_key = (
            observed.connector_id.clone(),
            observed.chat_id.clone(),
            observed.platform_message_id.clone(),
        );
        // A row already recorded as outgoing stays outgoing: the platform
        // reporting our own send back must not relabel it as incoming.
        if state
            .observed_messages
            .get(&message_key)
            .is_some_and(|existing| existing.direction == ObservedDirection::Outgoing)
        {
            observed.direction = ObservedDirection::Outgoing;
        }
        let chat_key = (observed.connector_id.clone(), observed.chat_id.clone());
        // Fold the message into the chat summary: keep the stored title unless
        // this message carried one, and advance the preview/timestamp only when
        // this message is at least as recent, so an older replayed message does
        // not rewind the summary.
        let summary = state
            .observed_chats
            .entry(chat_key)
            .or_insert_with(|| ObservedChat {
                connector_id: observed.connector_id.clone(),
                platform: observed.platform.clone(),
                chat_id: observed.chat_id.clone(),
                title: None,
                last_message_ts: i64::MIN,
                last_message_preview: String::new(),
            });
        if chat_title.is_some() {
            summary.title = chat_title;
        }
        if observed.timestamp_ms >= summary.last_message_ts {
            summary.last_message_ts = observed.timestamp_ms;
            summary.last_message_preview = observed.preview();
        }
        state
            .observed_messages
            .insert(message_key, observed.clone());
        Ok(observed)
    }

    async fn mark_observed_deleted(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        platform_message_id: &str,
    ) -> StorageResult<Option<ObservedMessage>> {
        let mut state = self.state.write().await;
        let observed = state.observed_messages.get_mut(&(
            connector_id.clone(),
            chat_id.to_owned(),
            platform_message_id.to_owned(),
        ));
        Ok(observed.map(|observed| {
            observed.deleted = true;
            observed.clone()
        }))
    }

    async fn mark_observed_edited(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        platform_message_id: &str,
        text: Option<String>,
    ) -> StorageResult<Option<ObservedMessage>> {
        let mut state = self.state.write().await;
        let observed = state.observed_messages.get_mut(&(
            connector_id.clone(),
            chat_id.to_owned(),
            platform_message_id.to_owned(),
        ));
        Ok(observed.map(|observed| {
            observed.text = text;
            observed.edited = true;
            observed.clone()
        }))
    }

    async fn rename_observed_chat(
        &self,
        connector_id: &ConnectorId,
        from_chat_id: &str,
        to_chat_id: &str,
    ) -> StorageResult<()> {
        if from_chat_id == to_chat_id {
            return Ok(());
        }
        let mut state = self.state.write().await;

        let moving: Vec<(ConnectorId, String, String)> = state
            .observed_messages
            .keys()
            .filter(|(connector, chat, _)| connector == connector_id && chat == from_chat_id)
            .cloned()
            .collect();
        for key in moving {
            let Some(mut observed) = state.observed_messages.remove(&key) else {
                continue;
            };
            observed.chat_id = to_chat_id.to_owned();
            // A row already logged under the destination wins: it is the form
            // the chat is known by now, and re-keying must not overwrite it.
            state
                .observed_messages
                .entry((key.0, to_chat_id.to_owned(), key.2))
                .or_insert(observed);
        }

        let Some(mut summary) = state
            .observed_chats
            .remove(&(connector_id.clone(), from_chat_id.to_owned()))
        else {
            return Ok(());
        };
        summary.chat_id = to_chat_id.to_owned();
        match state
            .observed_chats
            .get_mut(&(connector_id.clone(), to_chat_id.to_owned()))
        {
            // Both forms have a summary: keep the more recent activity, and keep
            // whichever title is known.
            Some(existing) => {
                if summary.last_message_ts > existing.last_message_ts {
                    existing.last_message_ts = summary.last_message_ts;
                    existing.last_message_preview = summary.last_message_preview;
                }
                if existing.title.is_none() {
                    existing.title = summary.title;
                }
            }
            None => {
                state
                    .observed_chats
                    .insert((connector_id.clone(), to_chat_id.to_owned()), summary);
            }
        }
        Ok(())
    }

    async fn list_observed_chats(&self) -> StorageResult<Vec<ObservedChat>> {
        let mut chats: Vec<ObservedChat> = self
            .state
            .read()
            .await
            .observed_chats
            .values()
            .cloned()
            .collect();
        chats.sort_by_key(|chat| std::cmp::Reverse(chat.last_message_ts));
        Ok(chats)
    }

    async fn observed_messages(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        before_ms: Option<i64>,
        limit: u32,
    ) -> StorageResult<Vec<ObservedMessage>> {
        let mut messages: Vec<ObservedMessage> = self
            .state
            .read()
            .await
            .observed_messages
            .values()
            .filter(|observed| {
                observed.connector_id == *connector_id
                    && observed.chat_id == chat_id
                    && before_ms.is_none_or(|before| observed.timestamp_ms < before)
            })
            .cloned()
            .collect();
        messages.sort_by_key(|observed| std::cmp::Reverse(observed.timestamp_ms));
        messages.truncate(limit as usize);
        Ok(messages)
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
