//! Persistence for the wuphf core: the [`Storage`] trait every backend
//! implements, plus the two backends behind it — an [`InMemoryStorage`] for
//! tests and a [`SqliteStorage`] for real runs.

mod memory;
mod shared;
mod sqlite;

pub use memory::InMemoryStorage;
pub use sqlite::SqliteStorage;

use async_trait::async_trait;
use std::collections::BTreeSet;
use thiserror::Error;
use wuphf_model::{
    AddressLink, Alias, ChatAddress, ChatProfile, ConnectorId, Conversation, ConversationId,
    DedupKey, Dispatch, DispatchId, Endpoint, EndpointId, HealthPause, MediaBlob, MediaId, Message,
    MessageCopy, MessageId, ObservedChat, ObservedMessage, Problem, RuleId,
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

/// Routing topology: endpoints, the aliases and address links that resolve
/// platform addresses to them, and the conversations that group them.
#[async_trait]
pub trait TopologyStore: Send + Sync + 'static {
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
    /// The chat id of the endpoint's canonical alias: the form the connector
    /// names the chat by now (the newest side of its last identity
    /// migration).
    async fn canonical_chat_id(&self, endpoint_id: EndpointId) -> StorageResult<Option<String>>;
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
    /// Record that the conversation's info card was created, leaving the rest
    /// of the conversation untouched.
    async fn mark_conversation_introduced(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<()>;
}

/// Logical messages and their per-endpoint copies, plus inbound dedup and the
/// sent-copy record that suppresses echoes.
#[async_trait]
pub trait MessageStore: Send + Sync + 'static {
    async fn claim_event(&self, key: DedupKey) -> StorageResult<bool>;
    async fn put_message(&self, message: Message) -> StorageResult<()>;
    async fn message(&self, message_id: MessageId) -> StorageResult<Option<Message>>;
    async fn put_received_copy(&self, copy: MessageCopy) -> StorageResult<()>;
    async fn record_sent_copy(&self, copy: MessageCopy) -> StorageResult<()>;
    async fn is_sent_copy(&self, endpoint_id: EndpointId, native_id: &str) -> StorageResult<bool>;
    async fn message_id_for_copy(
        &self,
        endpoint_id: EndpointId,
        native_id: &str,
    ) -> StorageResult<Option<MessageId>>;
    /// The message a copy names on any endpoint of one chat (any thread of a
    /// Telegram forum: a native id is unique within its chat). Resolves a
    /// reply that quotes a message in another thread or chat (plan 083).
    async fn message_id_for_chat_copy(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        native_id: &str,
    ) -> StorageResult<Option<MessageId>>;
    async fn copy_at_endpoint(
        &self,
        message_id: MessageId,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<MessageCopy>>;
    /// The copy of a message at the endpoint it was received on (its origin), if
    /// recorded. Used to target the origin with delivery acks and seen receipts.
    async fn received_copy(&self, message_id: MessageId) -> StorageResult<Option<MessageCopy>>;
}

/// Media blobs referenced by message attachments.
#[async_trait]
pub trait MediaStore: Send + Sync + 'static {
    async fn put_media(&self, media: MediaBlob, stored_at_ms: i64) -> StorageResult<()>;
    async fn media(&self, media_id: MediaId) -> StorageResult<Option<MediaBlob>>;
    async fn prune_unreferenced_media_before(&self, cutoff_ms: i64) -> StorageResult<u64>;
}

/// The outbound dispatch queue: enqueue, redeliver, and settle dispatches.
#[async_trait]
pub trait DispatchStore: Send + Sync + 'static {
    /// Persist a dispatch as given. The caller builds it with
    /// [`Dispatch::new`](wuphf_model::Dispatch::new), which already decides
    /// whether it is pending or blocked.
    async fn enqueue_dispatch(&self, dispatch: &Dispatch) -> StorageResult<()>;
    async fn pending_dispatches(&self) -> StorageResult<Vec<Dispatch>>;
    async fn dispatch(&self, dispatch_id: DispatchId) -> StorageResult<Option<Dispatch>>;
    async fn acknowledge_dispatch(
        &self,
        dispatch_id: DispatchId,
        native_id: String,
    ) -> StorageResult<Vec<Dispatch>>;
    /// Mark a dispatch as permanently failed so it leaves the pending queue and
    /// is never redelivered. Unlike acknowledgement, it records no sent copy.
    async fn fail_dispatch(&self, dispatch_id: DispatchId, reason: String) -> StorageResult<()>;
}

/// Messages held while a conversation's mirror thread is being provisioned,
/// durable so a crash cannot lose them.
#[async_trait]
pub trait ProvisioningStash: Send + Sync + 'static {
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
}

/// The observation log: every message a connector reports, for viewers such
/// as the TUI, independent of routing.
#[async_trait]
pub trait ObservationStore: Send + Sync + 'static {
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
        native_id: &str,
    ) -> StorageResult<Option<ObservedMessage>>;
    /// Replace an observed message's text and flag it edited, returning the
    /// updated row, or `None` when the message was never observed.
    async fn mark_observed_edited(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        native_id: &str,
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
    /// The observation log's summary of one chat, if it has seen the chat.
    async fn observed_chat(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
    ) -> StorageResult<Option<ObservedChat>>;
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

/// What the core knows about each platform chat, keyed by connector and the
/// chat's native id (threads are not part of the key).
#[async_trait]
pub trait ChatProfileStore: Send + Sync + 'static {
    /// The chat's profile, or `None` when nothing is known about it.
    async fn chat_profile(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
    ) -> StorageResult<Option<ChatProfile>>;
    /// Insert or replace the profile.
    async fn put_chat_profile(&self, profile: &ChatProfile) -> StorageResult<()>;
}

/// One stored runtime settings override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSetting {
    /// The dotted configuration key, e.g. `rules`.
    pub key: String,
    /// The JSON-encoded value.
    pub value: String,
    /// Who changed it, if known (a platform user id on a connector).
    pub updated_by: Option<String>,
}

/// Runtime settings overrides. Not part of [`Storage`]: the core never reads
/// settings; the settings store does, through `wuphf-settings`.
#[async_trait]
pub trait SettingsOverrides: Send + Sync + 'static {
    /// Every stored override, ordered by key.
    async fn settings_overrides(&self) -> StorageResult<Vec<StoredSetting>>;
    /// Delete the overrides of `deleted`, then store `upserted` (replacing any
    /// override of its key), all at once.
    async fn write_settings_overrides(
        &self,
        deleted: &[String],
        upserted: Option<&StoredSetting>,
        updated_at_ms: i64,
    ) -> StorageResult<()>;
}

/// The settings panel's button handles: actions too long for a platform's
/// button data, kept under a short key derived from the action
/// (botconf-panel's hybrid tokens). Not part of [`Storage`]: routing never
/// reads them.
#[async_trait]
pub trait PanelTokens: Send + Sync + 'static {
    /// Store `action` under `key` until `expires_at_ms`. If `key` holds the
    /// same action, or one that expired before `now_ms`, it is replaced
    /// (renewing the expiry) and the result is `true`. If it holds a
    /// different unexpired action (a hash collision), nothing changes and the
    /// result is `false`.
    async fn put_panel_token(
        &self,
        key: &str,
        action: &str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> StorageResult<bool>;
    /// The action under `key`, unless it is absent or expired at `now_ms`.
    async fn panel_token(&self, key: &str, now_ms: i64) -> StorageResult<Option<String>>;
    /// Delete the handles expired at `now_ms`; returns how many.
    async fn purge_panel_tokens(&self, now_ms: i64) -> StorageResult<u64>;
}

/// Who declared a runtime settings section: a connector when it registers
/// (plan 040), or a plugin such as wuphf-misc (plan 078).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SettingsOwner {
    Connector(ConnectorId),
    /// A plugin's id (e.g. `misc`).
    Plugin(String),
}

impl SettingsOwner {
    /// The owner's kind as stored: `connector` or `plugin`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Connector(_) => "connector",
            Self::Plugin(_) => "plugin",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Connector(connector) => &connector.0,
            Self::Plugin(plugin) => plugin,
        }
    }

    /// The owner from its stored kind and id; `None` for an unknown kind.
    pub fn from_parts(kind: &str, id: String) -> Option<Self> {
        match kind {
            "connector" => Some(Self::Connector(ConnectorId(id))),
            "plugin" => Some(Self::Plugin(id)),
            _ => None,
        }
    }
}

/// What a connector or a plugin last declared about its runtime settings: the
/// botconf descriptor of its settings, the JSON Schema of its section and its
/// own file values (the section's base).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsDeclaration {
    pub owner: SettingsOwner,
    /// A `botconf.v1.DescribeResponse`, serialized with protobuf.
    pub descriptor: Vec<u8>,
    pub json_schema: String,
    /// A JSON object.
    pub base_json: String,
}

/// Each connector's and plugin's last settings declaration, so its section
/// exists before it reconnects. Not part of [`Storage`]: routing never reads
/// it.
#[async_trait]
pub trait SettingsDeclarations: Send + Sync + 'static {
    /// Store `declaration`, replacing its owner's previous one.
    async fn put_settings_declaration(
        &self,
        declaration: &SettingsDeclaration,
        updated_at_ms: i64,
    ) -> StorageResult<()>;
    /// Every stored declaration, ordered by owner (connectors first, each
    /// kind by id).
    async fn settings_declarations(&self) -> StorageResult<Vec<SettingsDeclaration>>;
}

/// An open health problem and when it was first seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredProblem {
    pub problem: Problem,
    pub since_ms: i64,
}

/// A dispatch held for a paused direction of a rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldDispatch {
    pub dispatch: Dispatch,
    pub pause: HealthPause,
    pub held_at_ms: i64,
}

/// What the health checks keep (plan 079): the open problems, the ignored
/// ones, the dispatches held while a direction is paused, and the dead
/// letters to list.
#[async_trait]
pub trait HealthStore: Send + Sync + 'static {
    /// Every open problem, by key.
    async fn health_problems(&self) -> StorageResult<Vec<StoredProblem>>;
    /// Replace the open problems with `problems`.
    async fn set_health_problems(&self, problems: &[StoredProblem]) -> StorageResult<()>;
    /// The keys of the problems the owner ignores.
    async fn health_ignores(&self) -> StorageResult<BTreeSet<String>>;
    /// Ignore the problem `key`, or stop ignoring it.
    async fn set_health_ignored(&self, key: &str, ignored: bool) -> StorageResult<()>;
    /// Hold `dispatch` for `pause`, stamped with the time it was held.
    async fn hold_dispatch(&self, dispatch: &Dispatch, pause: &HealthPause) -> StorageResult<()>;
    /// Every held dispatch, in the order it was held.
    async fn held_dispatches(&self) -> StorageResult<Vec<HeldDispatch>>;
    /// Remove and return the dispatches held for `pause`, in the order they
    /// were held.
    async fn release_held(&self, pause: &HealthPause) -> StorageResult<Vec<HeldDispatch>>;
    /// Failed dispatches, newest first: `limit` of them after skipping
    /// `offset`.
    async fn dead_letters(&self, offset: usize, limit: usize) -> StorageResult<Vec<Dispatch>>;
}

/// One reactor's reaction on one copy of a message. `reactor` is `""` for
/// the connector's own session account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredReaction {
    pub reactor: String,
    pub emoji: String,
}

/// The reactions on each copy of a message (plan 080): the last reaction
/// each reactor has there, so a platform that reports a message's reactions
/// as a whole can be diffed, and a repeated or echoed reaction ignored.
#[async_trait]
pub trait ReactionStore: Send + Sync + 'static {
    /// The reactions on `message`'s copy at `endpoint`, ordered by reactor.
    async fn reactions_at(
        &self,
        message: MessageId,
        endpoint: EndpointId,
    ) -> StorageResult<Vec<StoredReaction>>;
    /// Set `reactor`'s reaction on that copy, or clear it with `None`.
    async fn set_reaction(
        &self,
        message: MessageId,
        endpoint: EndpointId,
        reactor: &str,
        emoji: Option<&str>,
    ) -> StorageResult<()>;
}

/// The full persistence surface the core needs: the sum of the role traits.
/// Blanket-implemented, so a backend implements the roles and gets `Storage`;
/// a consumer that needs only part of it bounds on the roles it uses.
pub trait Storage:
    TopologyStore
    + MessageStore
    + MediaStore
    + DispatchStore
    + ProvisioningStash
    + ObservationStore
    + ChatProfileStore
    + HealthStore
    + ReactionStore
{
}

impl<T> Storage for T where
    T: TopologyStore
        + MessageStore
        + MediaStore
        + DispatchStore
        + ProvisioningStash
        + ObservationStore
        + ChatProfileStore
        + HealthStore
        + ReactionStore
{
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod migration_tests;
