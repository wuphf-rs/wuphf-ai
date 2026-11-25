//! In-memory [`Storage`] backend. Keeps all state behind a single `RwLock`,
//! used for tests and ephemeral runs.

use crate::shared::{acknowledged_copy, release_blocked, wall_clock_ms};
use crate::{
    ChatProfileStore, DispatchStore, HealthStore, HeldDispatch, MediaStore, MessageStore,
    ObservationStore, PanelTokens, ProvisioningStash, ReactionStore, SettingsDeclaration,
    SettingsDeclarations, SettingsOverrides, StorageError, StorageResult, StoredProblem,
    StoredReaction, StoredSetting, TopologyStore,
};
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tokio::sync::RwLock;
use wuphf_model::{
    AddressLink, Alias, ChatAddress, ChatProfile, ConnectorId, Conversation, ConversationId,
    DedupKey, Dispatch, DispatchId, DispatchState, Endpoint, EndpointId, HealthPause, MediaBlob,
    MediaId, Message, MessageCopy, MessageId, ObservedChat, ObservedDirection, ObservedMessage,
    RuleId,
};

#[derive(Default)]
pub struct InMemoryStorage {
    state: RwLock<State>,
}

#[derive(Default)]
struct State {
    endpoints: HashMap<EndpointId, Endpoint>,
    aliases: HashMap<(ConnectorId, ChatAddress), Alias>,
    /// Symmetric address equivalences, mirroring the SQLite `address_links`
    /// table. Kept apart from `aliases` because a link exists with or without an
    /// endpoint behind either address.
    address_links: HashMap<(ConnectorId, ChatAddress), Vec<ChatAddress>>,
    conversations: HashMap<ConversationId, Conversation>,
    messages: HashMap<MessageId, Message>,
    copies: HashMap<(MessageId, EndpointId), MessageCopy>,
    copy_ids: HashMap<(EndpointId, String), MessageId>,
    sent_copies: HashSet<(EndpointId, String)>,
    seen_events: HashSet<DedupKey>,
    media: HashMap<MediaId, StoredMedia>,
    dispatches: HashMap<DispatchId, Dispatch>,
    /// Provisioning stash, keyed by conversation, insertion-ordered. Mirrors the
    /// SQLite `provisioning_stash` table; the message body is resolved from
    /// `messages` on drain.
    provisioning_stash: HashMap<ConversationId, Vec<(MessageId, EndpointId, String)>>,
    observed_messages: HashMap<(ConnectorId, String, String), ObservedMessage>,
    observed_chats: HashMap<(ConnectorId, String), ObservedChat>,
    /// Runtime settings overrides by key, mirroring the SQLite `settings` table
    /// (the write time is not kept: nothing reads it back).
    settings: BTreeMap<String, StoredSetting>,
    /// Settings-panel button handles: key to (action, expiry in ms), mirroring
    /// the SQLite `panel_tokens` table.
    panel_tokens: HashMap<String, (String, i64)>,
    /// Settings declarations by owner kind and id, mirroring the SQLite
    /// `settings_declarations` table (the write time is not kept).
    settings_declarations: BTreeMap<(&'static str, String), SettingsDeclaration>,
    /// Chat profiles by connector and native chat id, mirroring the SQLite
    /// `chat_profiles` table.
    chat_profiles: HashMap<(ConnectorId, String), ChatProfile>,
    /// Open health problems by key, mirroring `health_problems`.
    health_problems: BTreeMap<String, StoredProblem>,
    /// Ignored problem keys, mirroring `health_ignores`.
    health_ignores: BTreeSet<String>,
    /// Held dispatches in the order they were held, mirroring
    /// `held_dispatches`.
    held: Vec<HeldDispatch>,
    /// Reactions by copy, then reactor (ordered), mirroring `reactions`.
    reactions: HashMap<(MessageId, EndpointId), BTreeMap<String, String>>,
    /// Story expiries, mirroring `message_expiries`.
    expiries: HashMap<MessageId, i64>,
}

struct StoredMedia {
    blob: MediaBlob,
    stored_at_ms: i64,
}

#[async_trait]
impl TopologyStore for InMemoryStorage {
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
        address: &ChatAddress,
    ) -> StorageResult<Option<EndpointId>> {
        Ok(self
            .state
            .read()
            .await
            .aliases
            .get(&(connector_id.clone(), address.clone()))
            .map(|alias| alias.endpoint_id))
    }

    async fn canonical_chat_id(&self, endpoint_id: EndpointId) -> StorageResult<Option<String>> {
        Ok(self
            .state
            .read()
            .await
            .aliases
            .values()
            .find(|alias| alias.endpoint_id == endpoint_id && alias.is_canonical)
            .map(|alias| alias.address.chat_id.clone()))
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
        address: &ChatAddress,
    ) -> StorageResult<Vec<ChatAddress>> {
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

    async fn conversation_for_endpoint(
        &self,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<Conversation>> {
        Ok(self
            .state
            .read()
            .await
            .conversations
            .values()
            .find(|conversation| conversation.endpoints.contains(&endpoint_id))
            .cloned())
    }

    async fn conversation_for_source(
        &self,
        rule_id: &RuleId,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<Conversation>> {
        Ok(self
            .state
            .read()
            .await
            .conversations
            .values()
            .find(|conversation| {
                conversation.rule_id.as_ref() == Some(rule_id)
                    && conversation.source.connector_id == *connector_id
                    && conversation.source.address == *address
            })
            .cloned())
    }

    async fn add_conversation_endpoint(
        &self,
        conversation_id: ConversationId,
        endpoint_id: EndpointId,
    ) -> StorageResult<()> {
        let mut state = self.state.write().await;
        let conversation = state
            .conversations
            .get_mut(&conversation_id)
            .ok_or(StorageError::ConversationNotFound(conversation_id))?;
        if !conversation.endpoints.contains(&endpoint_id) {
            conversation.endpoints.push(endpoint_id);
        }
        Ok(())
    }

    async fn set_conversation_title(
        &self,
        conversation_id: ConversationId,
        title: &str,
    ) -> StorageResult<()> {
        self.state
            .write()
            .await
            .conversations
            .get_mut(&conversation_id)
            .ok_or(StorageError::ConversationNotFound(conversation_id))?
            .title = Some(title.to_owned());
        Ok(())
    }

    async fn mark_conversation_introduced(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<()> {
        self.state
            .write()
            .await
            .conversations
            .get_mut(&conversation_id)
            .ok_or(StorageError::ConversationNotFound(conversation_id))?
            .introduced = true;
        Ok(())
    }
}

#[async_trait]
impl MessageStore for InMemoryStorage {
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

    async fn is_sent_copy(&self, endpoint_id: EndpointId, native_id: &str) -> StorageResult<bool> {
        Ok(self
            .state
            .read()
            .await
            .sent_copies
            .contains(&(endpoint_id, native_id.to_owned())))
    }

    async fn message_id_for_copy(
        &self,
        endpoint_id: EndpointId,
        native_id: &str,
    ) -> StorageResult<Option<MessageId>> {
        Ok(self
            .state
            .read()
            .await
            .copy_ids
            .get(&(endpoint_id, native_id.to_owned()))
            .copied())
    }

    async fn message_id_for_chat_copy(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        native_id: &str,
    ) -> StorageResult<Option<MessageId>> {
        let state = self.state.read().await;
        Ok(state
            .copy_ids
            .iter()
            .find(|((endpoint_id, copy), _)| {
                copy == native_id
                    && state.endpoints.get(endpoint_id).is_some_and(|endpoint| {
                        endpoint.connector_id == *connector_id && endpoint.chat_id == chat_id
                    })
            })
            .map(|(_, message_id)| *message_id))
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
                        .contains(&(copy.endpoint_id, copy.native_id.clone()))
            })
            .map(|(_, copy)| copy.clone()))
    }

    async fn record_expiry(&self, message_id: MessageId, expires_at_ms: i64) -> StorageResult<()> {
        self.state
            .write()
            .await
            .expiries
            .insert(message_id, expires_at_ms);
        Ok(())
    }

    async fn due_expiries(&self, now_ms: i64, limit: usize) -> StorageResult<Vec<MessageId>> {
        let state = self.state.read().await;
        let mut due: Vec<_> = state
            .expiries
            .iter()
            .filter(|(_, at)| **at <= now_ms)
            .map(|(id, at)| (*at, *id))
            .collect();
        due.sort_by_key(|(at, id)| (*at, id.0));
        Ok(due.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    async fn forget_expiry(&self, message_id: MessageId) -> StorageResult<()> {
        self.state.write().await.expiries.remove(&message_id);
        Ok(())
    }
}

#[async_trait]
impl MediaStore for InMemoryStorage {
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
            // A chat's stored avatar is kept for its info cards.
            .chain(
                state
                    .chat_profiles
                    .values()
                    .filter_map(|profile| profile.avatar.as_ref()?.media_id),
            )
            .collect::<HashSet<_>>();
        let before = state.media.len();
        state
            .media
            .retain(|id, media| media.stored_at_ms >= cutoff_ms || referenced.contains(id));
        Ok((before - state.media.len()) as u64)
    }
}

#[async_trait]
impl DispatchStore for InMemoryStorage {
    async fn enqueue_dispatch(&self, dispatch: &Dispatch) -> StorageResult<()> {
        self.state
            .write()
            .await
            .dispatches
            .insert(dispatch.id, dispatch.clone());
        Ok(())
    }

    async fn pending_dispatches(&self) -> StorageResult<Vec<Dispatch>> {
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

    async fn dispatch(&self, dispatch_id: DispatchId) -> StorageResult<Option<Dispatch>> {
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
        native_id: String,
    ) -> StorageResult<Vec<Dispatch>> {
        let mut state = self.state.write().await;
        let dispatch = state
            .dispatches
            .get_mut(&dispatch_id)
            .ok_or(StorageError::DispatchNotFound(dispatch_id))?;
        dispatch.state = DispatchState::Acknowledged {
            native_id: native_id.clone(),
        };
        let Some(copy) = acknowledged_copy(dispatch, &native_id) else {
            return Ok(Vec::new());
        };
        let (message_id, endpoint_id) = (copy.message_id, copy.endpoint_id);
        put_copy(&mut state, copy, true);

        let blocked_ids: Vec<DispatchId> = state
            .dispatches
            .values()
            .filter(|candidate| {
                candidate.to.endpoint_id == endpoint_id
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
            release_blocked(candidate, parent.clone(), native_id.clone());
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

#[async_trait]
impl ProvisioningStash for InMemoryStorage {
    async fn stash_provisioning_message(
        &self,
        conversation_id: ConversationId,
        message_id: MessageId,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> StorageResult<()> {
        let mut state = self.state.write().await;
        let entries = state.provisioning_stash.entry(conversation_id).or_default();
        // Idempotent on (conversation_id, message_id), matching the SQLite PK.
        if entries.iter().any(|(id, _, _)| *id == message_id) {
            return Ok(());
        }
        entries.push((message_id, origin_endpoint, provision_title.to_owned()));
        Ok(())
    }

    async fn drain_provisioning_stash(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<Vec<(Message, EndpointId, String)>> {
        let mut state = self.state.write().await;
        let Some(entries) = state.provisioning_stash.remove(&conversation_id) else {
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

    async fn stashed_conversations(&self) -> StorageResult<Vec<ConversationId>> {
        Ok(self
            .state
            .read()
            .await
            .provisioning_stash
            .keys()
            .copied()
            .collect())
    }
}

#[async_trait]
impl ObservationStore for InMemoryStorage {
    async fn log_observed(
        &self,
        mut observed: ObservedMessage,
        chat_title: Option<String>,
    ) -> StorageResult<ObservedMessage> {
        let mut state = self.state.write().await;
        let message_key = (
            observed.connector_id.clone(),
            observed.chat_id.clone(),
            observed.native_id.clone(),
        );
        // A row already recorded as outgoing stays outgoing: the platform
        // reporting our own send back must not relabel it as incoming.
        if let Some(existing) = state
            .observed_messages
            .get(&message_key)
            .filter(|existing| existing.direction == ObservedDirection::Outgoing)
        {
            observed.direction = ObservedDirection::Outgoing;
            // So does the client that sent it: a client skips its own messages
            // by it, echoes included.
            if observed.sent_by.is_none() {
                observed.sent_by = existing.sent_by.clone();
            }
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
        native_id: &str,
    ) -> StorageResult<Option<ObservedMessage>> {
        let mut state = self.state.write().await;
        let observed = state.observed_messages.get_mut(&(
            connector_id.clone(),
            chat_id.to_owned(),
            native_id.to_owned(),
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
        native_id: &str,
        text: Option<String>,
    ) -> StorageResult<Option<ObservedMessage>> {
        let mut state = self.state.write().await;
        let observed = state.observed_messages.get_mut(&(
            connector_id.clone(),
            chat_id.to_owned(),
            native_id.to_owned(),
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

    async fn observed_chat(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
    ) -> StorageResult<Option<ObservedChat>> {
        Ok(self
            .state
            .read()
            .await
            .observed_chats
            .get(&(connector_id.clone(), chat_id.to_owned()))
            .cloned())
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

#[async_trait]
impl SettingsOverrides for InMemoryStorage {
    async fn settings_overrides(&self) -> StorageResult<Vec<StoredSetting>> {
        Ok(self.state.read().await.settings.values().cloned().collect())
    }

    async fn write_settings_overrides(
        &self,
        deleted: &[String],
        upserted: Option<&StoredSetting>,
        _updated_at_ms: i64,
    ) -> StorageResult<()> {
        let mut state = self.state.write().await;
        for key in deleted {
            state.settings.remove(key);
        }
        if let Some(setting) = upserted {
            state.settings.insert(setting.key.clone(), setting.clone());
        }
        Ok(())
    }
}

#[async_trait]
impl PanelTokens for InMemoryStorage {
    async fn put_panel_token(
        &self,
        key: &str,
        action: &str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> StorageResult<bool> {
        let mut state = self.state.write().await;
        if let Some((stored, expires_at)) = state.panel_tokens.get(key)
            && stored != action
            && *expires_at >= now_ms
        {
            return Ok(false);
        }
        state
            .panel_tokens
            .insert(key.to_owned(), (action.to_owned(), expires_at_ms));
        Ok(true)
    }

    async fn panel_token(&self, key: &str, now_ms: i64) -> StorageResult<Option<String>> {
        Ok(self
            .state
            .read()
            .await
            .panel_tokens
            .get(key)
            .filter(|(_, expires_at)| *expires_at >= now_ms)
            .map(|(action, _)| action.clone()))
    }

    async fn purge_panel_tokens(&self, now_ms: i64) -> StorageResult<u64> {
        let mut state = self.state.write().await;
        let before = state.panel_tokens.len();
        state
            .panel_tokens
            .retain(|_, (_, expires_at)| *expires_at >= now_ms);
        Ok((before - state.panel_tokens.len()) as u64)
    }
}

#[async_trait]
impl SettingsDeclarations for InMemoryStorage {
    async fn put_settings_declaration(
        &self,
        declaration: &SettingsDeclaration,
        _updated_at_ms: i64,
    ) -> StorageResult<()> {
        self.state.write().await.settings_declarations.insert(
            (declaration.owner.kind(), declaration.owner.id().to_owned()),
            declaration.clone(),
        );
        Ok(())
    }

    async fn settings_declarations(&self) -> StorageResult<Vec<SettingsDeclaration>> {
        Ok(self
            .state
            .read()
            .await
            .settings_declarations
            .values()
            .cloned()
            .collect())
    }
}

#[async_trait]
impl ChatProfileStore for InMemoryStorage {
    async fn chat_profile(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
    ) -> StorageResult<Option<ChatProfile>> {
        Ok(self
            .state
            .read()
            .await
            .chat_profiles
            .get(&(connector_id.clone(), chat_id.to_owned()))
            .cloned())
    }

    async fn put_chat_profile(&self, profile: &ChatProfile) -> StorageResult<()> {
        self.state.write().await.chat_profiles.insert(
            (profile.connector_id.clone(), profile.chat_id.clone()),
            profile.clone(),
        );
        Ok(())
    }
}

fn put_copy(state: &mut State, copy: MessageCopy, sent: bool) {
    if sent {
        state
            .sent_copies
            .insert((copy.endpoint_id, copy.native_id.clone()));
    }
    state
        .copy_ids
        .insert((copy.endpoint_id, copy.native_id.clone()), copy.message_id);
    state
        .copies
        .insert((copy.message_id, copy.endpoint_id), copy);
}

#[async_trait]
impl HealthStore for InMemoryStorage {
    async fn health_problems(&self) -> StorageResult<Vec<StoredProblem>> {
        Ok(self
            .state
            .read()
            .await
            .health_problems
            .values()
            .cloned()
            .collect())
    }

    async fn set_health_problems(&self, problems: &[StoredProblem]) -> StorageResult<()> {
        self.state.write().await.health_problems = problems
            .iter()
            .map(|stored| (stored.problem.key.clone(), stored.clone()))
            .collect();
        Ok(())
    }

    async fn health_ignores(&self) -> StorageResult<BTreeSet<String>> {
        Ok(self.state.read().await.health_ignores.clone())
    }

    async fn set_health_ignored(&self, key: &str, ignored: bool) -> StorageResult<()> {
        let mut state = self.state.write().await;
        if ignored {
            state.health_ignores.insert(key.to_owned());
        } else {
            state.health_ignores.remove(key);
        }
        Ok(())
    }

    async fn hold_dispatch(&self, dispatch: &Dispatch, pause: &HealthPause) -> StorageResult<()> {
        let mut state = self.state.write().await;
        state.held.retain(|held| held.dispatch.id != dispatch.id);
        state.held.push(HeldDispatch {
            dispatch: dispatch.clone(),
            pause: pause.clone(),
            held_at_ms: wall_clock_ms(),
        });
        Ok(())
    }

    async fn held_dispatches(&self) -> StorageResult<Vec<HeldDispatch>> {
        Ok(self.state.read().await.held.clone())
    }

    async fn release_held(&self, pause: &HealthPause) -> StorageResult<Vec<HeldDispatch>> {
        let mut state = self.state.write().await;
        let (released, kept) = std::mem::take(&mut state.held)
            .into_iter()
            .partition(|held| &held.pause == pause);
        state.held = kept;
        Ok(released)
    }

    async fn dead_letters(&self, offset: usize, limit: usize) -> StorageResult<Vec<Dispatch>> {
        // No insertion order here: newest first by id, which is enough for
        // tests.
        let mut failed: Vec<_> = self
            .state
            .read()
            .await
            .dispatches
            .values()
            .filter(|dispatch| matches!(dispatch.state, DispatchState::Failed { .. }))
            .cloned()
            .collect();
        failed.sort_by_key(|dispatch| std::cmp::Reverse(dispatch.id.0));
        Ok(failed.into_iter().skip(offset).take(limit).collect())
    }
}

#[async_trait]
impl ReactionStore for InMemoryStorage {
    async fn reactions_at(
        &self,
        message: MessageId,
        endpoint: EndpointId,
    ) -> StorageResult<Vec<StoredReaction>> {
        Ok(self
            .state
            .read()
            .await
            .reactions
            .get(&(message, endpoint))
            .map(|reactions| {
                reactions
                    .iter()
                    .map(|(reactor, emoji)| StoredReaction {
                        reactor: reactor.clone(),
                        emoji: emoji.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn set_reaction(
        &self,
        message: MessageId,
        endpoint: EndpointId,
        reactor: &str,
        emoji: Option<&str>,
    ) -> StorageResult<()> {
        let mut state = self.state.write().await;
        let reactions = state.reactions.entry((message, endpoint)).or_default();
        match emoji {
            Some(emoji) => {
                reactions.insert(reactor.to_owned(), emoji.to_owned());
            }
            None => {
                reactions.remove(reactor);
                if reactions.is_empty() {
                    state.reactions.remove(&(message, endpoint));
                }
            }
        }
        Ok(())
    }
}
