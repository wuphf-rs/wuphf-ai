//! Persistence for the wuphf core: the [`Storage`] trait every backend
//! implements, plus the two backends behind it — an [`InMemoryStorage`] for
//! tests and a [`SqliteStorage`] for real runs.

mod memory;
mod shared;
mod sqlite;

pub use memory::InMemoryStorage;
pub use sqlite::SqliteStorage;

use async_trait::async_trait;
use thiserror::Error;
use wuphf_model::{
    AddressLink, Alias, ChatAddress, ConnectorId, Conversation, ConversationId, DedupKey, Dispatch,
    DispatchId, Endpoint, EndpointId, MediaBlob, MediaId, Message, MessageCopy, MessageId,
    ObservedChat, ObservedMessage, RuleId,
};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("endpoint {0} is not configured")]
    EndpointNotFound(EndpointId),
    #[error("conversation {0} is not stored")]
    ConversationNotFound(ConversationId),
    #[error("dispatch {0} is not configured")]
    DispatchNotFound(DispatchId),
    #[error("media {0} is not stored")]
    MediaNotFound(MediaId),
    #[error("message {0} is not stored")]
    MessageNotFound(MessageId),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error("stored identifier is malformed: {0}")]
    Identifier(String),
}

pub type StorageResult<T> = Result<T, StorageError>;

#[async_trait]
pub trait Storage: Send + Sync + 'static {
    async fn put_endpoint(&self, endpoint: Endpoint) -> StorageResult<()>;
    async fn endpoint(&self, endpoint_id: EndpointId) -> StorageResult<Endpoint>;
    /// Insert or update an alias. Inserting a canonical alias clears the
    /// canonical flag on the endpoint's other aliases.
    async fn put_alias(&self, alias: Alias) -> StorageResult<()>;
    /// Resolve a platform address on a connector to its endpoint.
    async fn resolve_endpoint(
        &self,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<EndpointId>>;
    /// Record that two addresses on a connector name the same chat. Stored
    /// symmetrically, so a later lookup from either address finds the other.
    /// Independent of endpoints: a link is kept for chats that have never been
    /// routed, which is what lets a rule's chat list recognize a chat under a
    /// form the user did not write.
    async fn put_address_link(&self, link: AddressLink) -> StorageResult<()>;
    /// The addresses directly linked to this one on the connector. Callers walk
    /// these transitively to get a chat's full set of forms.
    async fn linked_addresses(
        &self,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Vec<ChatAddress>>;
    /// Insert or replace a conversation, including its endpoint list. Only for
    /// creating one: later changes go through the narrow writers below, so two
    /// workers updating different parts cannot overwrite each other.
    async fn put_conversation(&self, conversation: Conversation) -> StorageResult<()>;
    async fn conversation(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<Option<Conversation>>;
    /// The conversation an endpoint belongs to, if any. Indexed.
    async fn conversation_for_endpoint(
        &self,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<Conversation>>;
    /// The conversation a rule created for this source chat, if any.
    async fn conversation_for_source(
        &self,
        rule_id: &RuleId,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<Conversation>>;
    /// Append an endpoint to a conversation. Atomic and idempotent.
    async fn add_conversation_endpoint(
        &self,
        conversation_id: ConversationId,
        endpoint_id: EndpointId,
    ) -> StorageResult<()>;
    /// Change only a conversation's title, leaving its endpoints untouched.
    async fn set_conversation_title(
        &self,
        conversation_id: ConversationId,
        title: &str,
    ) -> StorageResult<()>;
    async fn claim_event(&self, key: DedupKey) -> StorageResult<bool>;
    async fn put_message(&self, message: Message) -> StorageResult<()>;
    async fn message(&self, message_id: MessageId) -> StorageResult<Option<Message>>;
    async fn put_received_copy(&self, copy: MessageCopy) -> StorageResult<()>;
    async fn record_sent_copy(&self, copy: MessageCopy) -> StorageResult<()>;
    async fn is_sent_copy(
        &self,
        endpoint_id: EndpointId,
        platform_message_id: &str,
    ) -> StorageResult<bool>;
    async fn message_id_for_copy(
        &self,
        endpoint_id: EndpointId,
        platform_message_id: &str,
    ) -> StorageResult<Option<MessageId>>;
    async fn copy_at_endpoint(
        &self,
        message_id: MessageId,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<MessageCopy>>;
    /// The copy of a message at the endpoint it was received on (its origin), if
    /// recorded. Used to target the origin with delivery acks and seen receipts.
    async fn received_copy(&self, message_id: MessageId) -> StorageResult<Option<MessageCopy>>;
    async fn put_media(&self, media: MediaBlob, stored_at_ms: i64) -> StorageResult<()>;
    async fn media(&self, media_id: MediaId) -> StorageResult<Option<MediaBlob>>;
    async fn prune_unreferenced_media_before(&self, cutoff_ms: i64) -> StorageResult<u64>;
    /// Persist a dispatch as given. The caller builds it with
    /// [`Dispatch::new`](wuphf_model::Dispatch::new), which already decides
    /// whether it is pending or blocked.
    async fn enqueue_dispatch(&self, dispatch: &Dispatch) -> StorageResult<()>;
    async fn pending_dispatches(&self) -> StorageResult<Vec<Dispatch>>;
    async fn dispatch(&self, dispatch_id: DispatchId) -> StorageResult<Option<Dispatch>>;
    async fn acknowledge_dispatch(
        &self,
        dispatch_id: DispatchId,
        platform_message_id: String,
    ) -> StorageResult<Vec<Dispatch>>;
    /// Mark a dispatch as permanently failed so it leaves the pending queue and
    /// is never redelivered. Unlike acknowledgement, it records no sent copy.
    async fn fail_dispatch(&self, dispatch_id: DispatchId, reason: String) -> StorageResult<()>;

    /// Persist one provisioning-deferred message for later replay, so a crash
    /// while a mirror thread is being provisioned cannot lose it. Idempotent on
    /// `(conversation_id, message_id)`.
    async fn stash_provisioning_message(
        &self,
        conversation_id: ConversationId,
        message_id: MessageId,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> StorageResult<()>;
    /// Remove and return the stashed messages for a conversation, in insertion
    /// order, resolved back to full messages (rows whose message is no longer
    /// stored are dropped). The removal and the read are one atomic step.
    async fn drain_provisioning_stash(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<Vec<(Message, EndpointId, String)>>;
    /// Conversations that still have stashed messages, for startup recovery.
    async fn stashed_conversations(&self) -> StorageResult<Vec<ConversationId>>;

    /// Record a message in the observation log, upserting both the message row
    /// and its chat summary, and return the row as stored. Idempotent on the
    /// message's native coordinates. A `chat_title` refreshes the chat's title;
    /// `None` keeps the stored one, so a later message that omits a title does
    /// not clear it. A row already recorded as outgoing keeps that direction:
    /// the platform reporting the bridge's own send back as an inbound event
    /// must not relabel it as incoming.
    async fn log_observed(
        &self,
        observed: ObservedMessage,
        chat_title: Option<String>,
    ) -> StorageResult<ObservedMessage>;
    /// Flag an observed message deleted, returning the updated row, or `None`
    /// when the message was never observed.
    async fn mark_observed_deleted(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        platform_message_id: &str,
    ) -> StorageResult<Option<ObservedMessage>>;
    /// Replace an observed message's text and flag it edited, returning the
    /// updated row, or `None` when the message was never observed.
    async fn mark_observed_edited(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        platform_message_id: &str,
        text: Option<String>,
    ) -> StorageResult<Option<ObservedMessage>>;
    /// Re-key every observation row of one chat onto another address on the
    /// same connector, merging into whatever is already recorded there.
    ///
    /// The observation log is written before routing so it can capture chats no
    /// rule matches, which means it never passes through the endpoint/alias
    /// layer that keeps the rest of the core address-agnostic. When a platform
    /// later reveals that two addresses are one chat, the rows already logged
    /// under the old address would otherwise stay a separate conversation
    /// forever. Applying the rename heals that split retroactively.
    async fn rename_observed_chat(
        &self,
        connector_id: &ConnectorId,
        from_chat_id: &str,
        to_chat_id: &str,
    ) -> StorageResult<()>;
    /// Every chat the observation log has seen, most recently active first.
    async fn list_observed_chats(&self) -> StorageResult<Vec<ObservedChat>>;
    /// A page of a chat's observed messages, newest first, older than `before_ms`
    /// when set (for scrollback), capped at `limit`.
    async fn observed_messages(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        before_ms: Option<i64>,
        limit: u32,
    ) -> StorageResult<Vec<ObservedMessage>>;
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod migration_tests;
