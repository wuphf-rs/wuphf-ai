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
    Alias, ChatSettings, ConnectorId, Contact, Conversation, ConversationId, DedupKey,
    DispatchCommand, DispatchId, Endpoint, EndpointId, MediaBlob, MediaId, Message, MessageCopy,
    MessageId, ObservedChat, ObservedMessage, PlatformAddress, ReplicaSet, ReplicaSetId,
    StoredDispatch, ThreadMapping,
};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("endpoint {0} is not configured")]
    EndpointNotFound(EndpointId),
    #[error("replica set {0} is not configured")]
    ReplicaSetNotFound(ReplicaSetId),
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
    async fn put_replica_set(&self, replica_set: ReplicaSet) -> StorageResult<()>;
    async fn replica_set(&self, replica_set_id: ReplicaSetId) -> StorageResult<ReplicaSet>;
    /// The replica set that contains the endpoint, if any.
    async fn replica_set_for_endpoint(
        &self,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<ReplicaSetId>>;
    /// Insert or update an alias. Inserting a canonical alias clears the
    /// canonical flag on the endpoint's other aliases.
    async fn put_alias(&self, alias: Alias) -> StorageResult<()>;
    /// Resolve a platform address on a connector to its endpoint.
    async fn resolve_endpoint(
        &self,
        connector_id: &ConnectorId,
        address: &PlatformAddress,
    ) -> StorageResult<Option<EndpointId>>;
    async fn put_conversation(&self, conversation: Conversation) -> StorageResult<()>;
    async fn conversation(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<Option<Conversation>>;
    /// The conversation whose identity source is this address, if any.
    async fn conversation_for_source(
        &self,
        rule_id: &wuphf_model::RuleId,
        connector_id: &ConnectorId,
        address: &PlatformAddress,
    ) -> StorageResult<Option<Conversation>>;
    /// The conversation backed by this replica set, if any.
    async fn conversation_for_replica_set(
        &self,
        replica_set_id: ReplicaSetId,
    ) -> StorageResult<Option<Conversation>>;
    async fn put_thread_mapping(&self, mapping: ThreadMapping) -> StorageResult<()>;
    /// The thread provisioned for a conversation in the given connector's
    /// container, if one exists yet.
    async fn thread_for_conversation(
        &self,
        conversation_id: ConversationId,
        connector_id: &ConnectorId,
    ) -> StorageResult<Option<String>>;
    /// Reverse lookup for a message typed directly into a mirror topic.
    async fn conversation_for_thread(
        &self,
        connector_id: &ConnectorId,
        container: &str,
        thread_id: &str,
    ) -> StorageResult<Option<ConversationId>>;
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
    async fn put_contact(&self, contact: Contact) -> StorageResult<()>;
    async fn put_chat_settings(&self, settings: ChatSettings) -> StorageResult<()>;
    async fn put_media(&self, media: MediaBlob, stored_at_ms: i64) -> StorageResult<()>;
    async fn media(&self, media_id: MediaId) -> StorageResult<Option<MediaBlob>>;
    async fn prune_unreferenced_media_before(&self, cutoff_ms: i64) -> StorageResult<u64>;
    async fn enqueue_dispatch(
        &self,
        command: DispatchCommand,
        waiting_for: Option<MessageId>,
    ) -> StorageResult<StoredDispatch>;
    async fn pending_dispatches(&self) -> StorageResult<Vec<StoredDispatch>>;
    async fn dispatch(&self, dispatch_id: DispatchId) -> StorageResult<Option<StoredDispatch>>;
    async fn acknowledge_dispatch(
        &self,
        dispatch_id: DispatchId,
        platform_message_id: String,
    ) -> StorageResult<Vec<StoredDispatch>>;
    /// Mark a dispatch as permanently failed so it leaves the pending queue and
    /// is never redelivered. Unlike acknowledgement, it records no sent copy.
    async fn fail_dispatch(&self, dispatch_id: DispatchId, reason: String) -> StorageResult<()>;

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
