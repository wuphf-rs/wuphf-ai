//! SQLite-backed [`Storage`]. Rows hold JSON-encoded model values keyed by id;
//! dispatch state is denormalized into columns so pending work can be queried.

use crate::shared::{acknowledged_copy, release_blocked};
use crate::{
    ChatProfileStore, DispatchStore, MediaStore, MessageStore, ObservationStore, PanelTokens,
    ProvisioningStash, SettingsDeclaration, SettingsDeclarations, SettingsOverrides, StorageError,
    StorageResult, StoredSetting, TopologyStore,
};
use async_trait::async_trait;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use wuphf_model::{
    AddressLink, Alias, ChatAddress, ChatProfile, ConnectorId, Conversation, ConversationId,
    DedupKey, Dispatch, DispatchId, DispatchState, Endpoint, EndpointId, MediaBlob, MediaId,
    Message, MessageCopy, MessageId, ObservedChat, ObservedDirection, ObservedMessage, RuleId,
};

/// The schema migrations, embedded at build time. Shared with the migration
/// tests so they can stop partway and seed rows in an older shape.
pub(crate) static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub async fn connect(database_url: &str) -> StorageResult<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    /// The file a database URL names (`sqlite://wuphf.sqlite` is
    /// `wuphf.sqlite`); `None` for an in-memory database or a URL that does
    /// not parse.
    pub fn database_file(database_url: &str) -> Option<std::path::PathBuf> {
        // sqlx names an in-memory database after a placeholder file.
        if database_url.contains(":memory:") || database_url.contains("mode=memory") {
            return None;
        }
        let options = SqliteConnectOptions::from_str(database_url).ok()?;
        let file = options.get_filename();
        (!file.as_os_str().is_empty()).then(|| file.to_owned())
    }
}

#[async_trait]
impl TopologyStore for SqliteStorage {
    async fn put_endpoint(&self, endpoint: Endpoint) -> StorageResult<()> {
        upsert_json(
            &self.pool,
            "endpoints",
            "id",
            endpoint.id.to_string(),
            &endpoint,
        )
        .await
    }

    async fn endpoint(&self, endpoint_id: EndpointId) -> StorageResult<Endpoint> {
        get_json(&self.pool, "endpoints", "id", endpoint_id.to_string())
            .await?
            .ok_or(StorageError::EndpointNotFound(endpoint_id))
    }

    async fn put_alias(&self, alias: Alias) -> StorageResult<()> {
        let mut transaction = self.pool.begin().await?;
        if alias.is_canonical {
            sqlx::query("UPDATE aliases SET is_canonical = 0 WHERE endpoint_id = ?")
                .bind(alias.endpoint_id.to_string())
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query(
            "INSERT INTO aliases (connector_id, chat_id, thread_key, endpoint_id, is_canonical)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(connector_id, chat_id, thread_key) DO UPDATE SET
               endpoint_id = excluded.endpoint_id,
               is_canonical = excluded.is_canonical",
        )
        .bind(alias.connector_id.to_string())
        .bind(&alias.address.chat_id)
        .bind(thread_key(&alias.address))
        .bind(alias.endpoint_id.to_string())
        .bind(i64::from(alias.is_canonical))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn resolve_endpoint(
        &self,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<EndpointId>> {
        let row = sqlx::query(
            "SELECT endpoint_id FROM aliases WHERE connector_id = ? AND chat_id = ? AND thread_key = ?",
        )
        .bind(connector_id.to_string())
        .bind(&address.chat_id)
        .bind(thread_key(address))
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| parse_endpoint_id(row.get("endpoint_id")))
            .transpose()
    }

    async fn canonical_chat_id(&self, endpoint_id: EndpointId) -> StorageResult<Option<String>> {
        let row = sqlx::query(
            "SELECT chat_id FROM aliases WHERE endpoint_id = ? AND is_canonical = 1 LIMIT 1",
        )
        .bind(endpoint_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get("chat_id")))
    }

    async fn put_address_link(&self, link: AddressLink) -> StorageResult<()> {
        if link.address == link.linked_address {
            return Ok(());
        }
        let mut transaction = self.pool.begin().await?;
        for (from, to) in [
            (&link.address, &link.linked_address),
            (&link.linked_address, &link.address),
        ] {
            sqlx::query(
                "INSERT INTO address_links
                   (connector_id, chat_id, thread_key, linked_chat_id, linked_thread_key)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )
            .bind(link.connector_id.to_string())
            .bind(&from.chat_id)
            .bind(thread_key(from))
            .bind(&to.chat_id)
            .bind(thread_key(to))
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn linked_addresses(
        &self,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Vec<ChatAddress>> {
        let rows = sqlx::query(
            "SELECT linked_chat_id, linked_thread_key FROM address_links
             WHERE connector_id = ? AND chat_id = ? AND thread_key = ?
             ORDER BY rowid",
        )
        .bind(connector_id.to_string())
        .bind(&address.chat_id)
        .bind(thread_key(address))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let thread: String = row.get("linked_thread_key");
                ChatAddress::new(
                    row.get::<String, _>("linked_chat_id"),
                    (!thread.is_empty()).then_some(thread),
                )
            })
            .collect())
    }

    async fn put_conversation(&self, conversation: Conversation) -> StorageResult<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO conversations
               (id, rule_id, source_connector, source_chat_id, source_thread_key, payload)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
        )
        .bind(conversation.id.to_string())
        .bind(conversation.rule_id.as_ref().map(ToString::to_string))
        .bind(conversation.source.connector_id.to_string())
        .bind(&conversation.source.address.chat_id)
        .bind(thread_key(&conversation.source.address))
        .bind(serde_json::to_string(&conversation)?)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM conversation_endpoints WHERE conversation_id = ?")
            .bind(conversation.id.to_string())
            .execute(&mut *transaction)
            .await?;
        for endpoint_id in &conversation.endpoints {
            insert_conversation_endpoint(&mut transaction, conversation.id, *endpoint_id).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn conversation(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<Option<Conversation>> {
        get_json(
            &self.pool,
            "conversations",
            "id",
            conversation_id.to_string(),
        )
        .await
    }

    async fn conversation_for_endpoint(
        &self,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<Conversation>> {
        let row = sqlx::query(
            "SELECT conversations.payload
             FROM conversation_endpoints
             JOIN conversations ON conversations.id = conversation_endpoints.conversation_id
             WHERE conversation_endpoints.endpoint_id = ?",
        )
        .bind(endpoint_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| serde_json::from_str(row.get("payload")))
            .transpose()
            .map_err(Into::into)
    }

    async fn conversation_for_source(
        &self,
        rule_id: &RuleId,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<Conversation>> {
        let row = sqlx::query(
            "SELECT payload FROM conversations
             WHERE rule_id = ? AND source_connector = ? AND source_chat_id = ? AND source_thread_key = ?",
        )
        .bind(rule_id.to_string())
        .bind(connector_id.to_string())
        .bind(&address.chat_id)
        .bind(thread_key(address))
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| serde_json::from_str(row.get("payload")))
            .transpose()
            .map_err(Into::into)
    }

    async fn add_conversation_endpoint(
        &self,
        conversation_id: ConversationId,
        endpoint_id: EndpointId,
    ) -> StorageResult<()> {
        let mut transaction = self.pool.begin().await?;
        let mut conversation = conversation_in(&mut transaction, conversation_id).await?;
        if !conversation.endpoints.contains(&endpoint_id) {
            conversation.endpoints.push(endpoint_id);
            update_conversation_payload(&mut transaction, &conversation).await?;
        }
        insert_conversation_endpoint(&mut transaction, conversation_id, endpoint_id).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn set_conversation_title(
        &self,
        conversation_id: ConversationId,
        title: &str,
    ) -> StorageResult<()> {
        let mut transaction = self.pool.begin().await?;
        let mut conversation = conversation_in(&mut transaction, conversation_id).await?;
        conversation.title = Some(title.to_owned());
        update_conversation_payload(&mut transaction, &conversation).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn mark_conversation_introduced(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<()> {
        let mut transaction = self.pool.begin().await?;
        let mut conversation = conversation_in(&mut transaction, conversation_id).await?;
        conversation.introduced = true;
        update_conversation_payload(&mut transaction, &conversation).await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl MessageStore for SqliteStorage {
    async fn claim_event(&self, key: DedupKey) -> StorageResult<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO seen_events (platform, chat_id, platform_message_id) VALUES (?, ?, ?)",
        )
        .bind(serde_json::to_string(&key.platform)?)
        .bind(key.chat_id)
        .bind(key.native_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn put_message(&self, message: Message) -> StorageResult<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO messages (id, payload) VALUES (?, ?)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
        )
        .bind(message.id.to_string())
        .bind(serde_json::to_string(&message)?)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM message_media WHERE message_id = ?")
            .bind(message.id.to_string())
            .execute(&mut *transaction)
            .await?;
        for attachment in &message.attachments {
            sqlx::query("INSERT INTO message_media (message_id, media_id) VALUES (?, ?)")
                .bind(message.id.to_string())
                .bind(attachment.media_id.to_string())
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn message(&self, message_id: MessageId) -> StorageResult<Option<Message>> {
        get_json(&self.pool, "messages", "id", message_id.to_string()).await
    }

    async fn put_received_copy(&self, copy: MessageCopy) -> StorageResult<()> {
        put_sqlite_copy(&self.pool, copy, false).await
    }

    async fn record_sent_copy(&self, copy: MessageCopy) -> StorageResult<()> {
        put_sqlite_copy(&self.pool, copy, true).await
    }

    async fn is_sent_copy(&self, endpoint_id: EndpointId, native_id: &str) -> StorageResult<bool> {
        let row = sqlx::query(
            "SELECT sent FROM message_copies WHERE endpoint_id = ? AND platform_message_id = ?",
        )
        .bind(endpoint_id.to_string())
        .bind(native_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some_and(|row| row.get::<i64, _>("sent") != 0))
    }

    async fn message_id_for_copy(
        &self,
        endpoint_id: EndpointId,
        native_id: &str,
    ) -> StorageResult<Option<MessageId>> {
        let row = sqlx::query(
            "SELECT payload FROM message_copies WHERE endpoint_id = ? AND platform_message_id = ?",
        )
        .bind(endpoint_id.to_string())
        .bind(native_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| serde_json::from_str::<MessageCopy>(row.get("payload")))
            .transpose()
            .map(|copy| copy.map(|copy| copy.message_id))
            .map_err(Into::into)
    }

    async fn copy_at_endpoint(
        &self,
        message_id: MessageId,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<MessageCopy>> {
        let row = sqlx::query(
            "SELECT payload FROM message_copies WHERE message_id = ? AND endpoint_id = ?",
        )
        .bind(message_id.to_string())
        .bind(endpoint_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| serde_json::from_str(row.get("payload")))
            .transpose()
            .map_err(Into::into)
    }

    async fn received_copy(&self, message_id: MessageId) -> StorageResult<Option<MessageCopy>> {
        let row = sqlx::query(
            "SELECT payload FROM message_copies WHERE message_id = ? AND sent = 0 LIMIT 1",
        )
        .bind(message_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| serde_json::from_str(row.get("payload")))
            .transpose()
            .map_err(Into::into)
    }
}

#[async_trait]
impl MediaStore for SqliteStorage {
    async fn put_media(&self, media: MediaBlob, stored_at_ms: i64) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO media_blobs (id, metadata, contents, stored_at_ms) VALUES (?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               metadata = excluded.metadata,
               contents = excluded.contents,
               stored_at_ms = excluded.stored_at_ms",
        )
        .bind(media.attachment.media_id.to_string())
        .bind(serde_json::to_string(&media.attachment)?)
        .bind(media.bytes)
        .bind(stored_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn media(&self, media_id: MediaId) -> StorageResult<Option<MediaBlob>> {
        let row = sqlx::query("SELECT metadata, contents FROM media_blobs WHERE id = ?")
            .bind(media_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            Ok(MediaBlob {
                attachment: serde_json::from_str(row.get("metadata"))?,
                bytes: row.get("contents"),
            })
        })
        .transpose()
    }

    async fn prune_unreferenced_media_before(&self, cutoff_ms: i64) -> StorageResult<u64> {
        let result = sqlx::query(
            "DELETE FROM media_blobs
             WHERE stored_at_ms IS NOT NULL
               AND stored_at_ms < ?
               AND NOT EXISTS (
                 SELECT 1 FROM message_media WHERE message_media.media_id = media_blobs.id
               )
               AND NOT EXISTS (
                 SELECT 1 FROM chat_profiles
                 WHERE chat_profiles.payload ->> '$.avatar.media_id' = media_blobs.id
               )",
        )
        .bind(cutoff_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[async_trait]
impl DispatchStore for SqliteStorage {
    async fn enqueue_dispatch(&self, dispatch: &Dispatch) -> StorageResult<()> {
        insert_dispatch(&self.pool, dispatch).await
    }

    async fn pending_dispatches(&self) -> StorageResult<Vec<Dispatch>> {
        let rows =
            sqlx::query("SELECT payload FROM dispatches WHERE state = 'pending' ORDER BY rowid")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|row| serde_json::from_str(row.get("payload")).map_err(Into::into))
            .collect()
    }

    async fn dispatch(&self, dispatch_id: DispatchId) -> StorageResult<Option<Dispatch>> {
        get_json(&self.pool, "dispatches", "id", dispatch_id.to_string()).await
    }

    async fn acknowledge_dispatch(
        &self,
        dispatch_id: DispatchId,
        native_id: String,
    ) -> StorageResult<Vec<Dispatch>> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT payload FROM dispatches WHERE id = ?")
            .bind(dispatch_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StorageError::DispatchNotFound(dispatch_id))?;
        let mut dispatch: Dispatch = serde_json::from_str(row.get("payload"))?;
        dispatch.state = DispatchState::Acknowledged {
            native_id: native_id.clone(),
        };
        sqlx::query("UPDATE dispatches SET waiting_for = ?, state = ?, payload = ? WHERE id = ?")
            .bind(None::<String>)
            .bind(state_name(&dispatch.state))
            .bind(serde_json::to_string(&dispatch)?)
            .bind(dispatch.id.to_string())
            .execute(&mut *transaction)
            .await?;
        let Some(copy) = acknowledged_copy(&dispatch, &native_id) else {
            transaction.commit().await?;
            return Ok(Vec::new());
        };
        let message_id = copy.message_id;
        sqlx::query(
            "INSERT INTO message_copies (message_id, endpoint_id, platform_message_id, sent, payload)
             VALUES (?, ?, ?, 1, ?)
             ON CONFLICT(message_id, endpoint_id) DO UPDATE SET
               platform_message_id = excluded.platform_message_id,
               sent = 1,
               payload = excluded.payload",
        )
        .bind(copy.message_id.to_string())
        .bind(copy.endpoint_id.to_string())
        .bind(&copy.native_id)
        .bind(serde_json::to_string(&copy)?)
        .execute(&mut *transaction)
        .await?;

        let rows = sqlx::query(
            "SELECT payload FROM dispatches WHERE state = 'blocked' AND waiting_for = ? AND target_endpoint = ?",
        )
        .bind(message_id.to_string())
        .bind(dispatch.to.endpoint_id.to_string())
        .fetch_all(&mut *transaction)
        .await?;
        let mut released = Vec::new();
        if rows.is_empty() {
            transaction.commit().await?;
            return Ok(released);
        }
        let parent_row = sqlx::query("SELECT payload FROM messages WHERE id = ?")
            .bind(message_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StorageError::MessageNotFound(message_id))?;
        let parent: Message = serde_json::from_str(parent_row.get("payload"))?;
        for row in rows {
            let mut blocked: Dispatch = serde_json::from_str(row.get("payload"))?;
            release_blocked(&mut blocked, parent.clone(), native_id.clone());
            sqlx::query(
                "UPDATE dispatches SET waiting_for = ?, state = ?, payload = ? WHERE id = ?",
            )
            .bind(None::<String>)
            .bind(state_name(&blocked.state))
            .bind(serde_json::to_string(&blocked)?)
            .bind(blocked.id.to_string())
            .execute(&mut *transaction)
            .await?;
            released.push(blocked);
        }
        transaction.commit().await?;
        Ok(released)
    }

    async fn fail_dispatch(&self, dispatch_id: DispatchId, reason: String) -> StorageResult<()> {
        let row = sqlx::query("SELECT payload FROM dispatches WHERE id = ?")
            .bind(dispatch_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StorageError::DispatchNotFound(dispatch_id))?;
        let mut dispatch: Dispatch = serde_json::from_str(row.get("payload"))?;
        dispatch.state = DispatchState::Failed { reason };
        sqlx::query("UPDATE dispatches SET waiting_for = ?, state = ?, payload = ? WHERE id = ?")
            .bind(None::<String>)
            .bind(state_name(&dispatch.state))
            .bind(serde_json::to_string(&dispatch)?)
            .bind(dispatch.id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ProvisioningStash for SqliteStorage {
    async fn stash_provisioning_message(
        &self,
        conversation_id: ConversationId,
        message_id: MessageId,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO provisioning_stash
                 (conversation_id, message_id, origin_endpoint, provision_title)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(conversation_id, message_id) DO NOTHING",
        )
        .bind(conversation_id.to_string())
        .bind(message_id.to_string())
        .bind(origin_endpoint.to_string())
        .bind(provision_title)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn drain_provisioning_stash(
        &self,
        conversation_id: ConversationId,
    ) -> StorageResult<Vec<(Message, EndpointId, String)>> {
        let mut transaction = self.pool.begin().await?;
        // Join the message body back on; ORDER BY rowid preserves insertion
        // order. Rows whose message vanished are dropped by the inner join.
        let rows = sqlx::query(
            "SELECT stash.origin_endpoint, stash.provision_title, messages.payload AS message
             FROM provisioning_stash AS stash
             JOIN messages ON messages.id = stash.message_id
             WHERE stash.conversation_id = ?
             ORDER BY stash.rowid",
        )
        .bind(conversation_id.to_string())
        .fetch_all(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM provisioning_stash WHERE conversation_id = ?")
            .bind(conversation_id.to_string())
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;

        let mut drained = Vec::with_capacity(rows.len());
        for row in rows {
            let message: Message = serde_json::from_str(row.get("message"))?;
            let origin_endpoint = parse_endpoint_id(row.get("origin_endpoint"))?;
            let provision_title: String = row.get("provision_title");
            drained.push((message, origin_endpoint, provision_title));
        }
        Ok(drained)
    }

    async fn stashed_conversations(&self) -> StorageResult<Vec<ConversationId>> {
        let rows = sqlx::query("SELECT DISTINCT conversation_id FROM provisioning_stash")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                uuid::Uuid::parse_str(row.get("conversation_id"))
                    .map(ConversationId)
                    .map_err(|err| StorageError::Identifier(err.to_string()))
            })
            .collect()
    }
}

#[async_trait]
impl ObservationStore for SqliteStorage {
    async fn log_observed(
        &self,
        mut observed: ObservedMessage,
        chat_title: Option<String>,
    ) -> StorageResult<ObservedMessage> {
        let mut transaction = self.pool.begin().await?;
        // A row already recorded as outgoing stays outgoing: the platform
        // reporting our own send back must not relabel it as incoming.
        let existing_message = sqlx::query(
            "SELECT payload FROM observed_messages
             WHERE connector_id = ? AND chat_id = ? AND platform_message_id = ?",
        )
        .bind(observed.connector_id.to_string())
        .bind(&observed.chat_id)
        .bind(&observed.native_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = existing_message {
            let stored: ObservedMessage = serde_json::from_str(row.get("payload"))?;
            if stored.direction == ObservedDirection::Outgoing {
                observed.direction = ObservedDirection::Outgoing;
                // So does the client that sent it: a client skips its own
                // messages by it, echoes included.
                observed.sent_by = observed.sent_by.or(stored.sent_by);
            }
        }
        sqlx::query(
            "INSERT INTO observed_messages
               (connector_id, chat_id, platform_message_id, timestamp_ms, payload)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(connector_id, chat_id, platform_message_id) DO UPDATE SET
               timestamp_ms = excluded.timestamp_ms,
               payload = excluded.payload",
        )
        .bind(observed.connector_id.to_string())
        .bind(&observed.chat_id)
        .bind(&observed.native_id)
        .bind(observed.timestamp_ms)
        .bind(serde_json::to_string(&observed)?)
        .execute(&mut *transaction)
        .await?;

        // Fold the message into the chat summary. The title updates only when the
        // message carried one (COALESCE keeps the stored title otherwise); the
        // preview and timestamp advance only when this message is at least as
        // recent as the stored one, so an out-of-order replay of an older message
        // does not rewind the summary.
        let summary = ObservedChat {
            connector_id: observed.connector_id.clone(),
            platform: observed.platform.clone(),
            chat_id: observed.chat_id.clone(),
            title: chat_title.clone(),
            last_message_ts: observed.timestamp_ms,
            last_message_preview: observed.preview(),
        };
        let existing = sqlx::query(
            "SELECT payload FROM observed_chats WHERE connector_id = ? AND chat_id = ?",
        )
        .bind(observed.connector_id.to_string())
        .bind(&observed.chat_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let merged = match existing {
            Some(row) => {
                let mut stored: ObservedChat = serde_json::from_str(row.get("payload"))?;
                if chat_title.is_some() {
                    stored.title = chat_title;
                }
                if summary.last_message_ts >= stored.last_message_ts {
                    stored.last_message_ts = summary.last_message_ts;
                    stored.last_message_preview = summary.last_message_preview;
                }
                stored
            }
            None => summary,
        };
        sqlx::query(
            "INSERT INTO observed_chats (connector_id, chat_id, last_message_ts, payload)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(connector_id, chat_id) DO UPDATE SET
               last_message_ts = excluded.last_message_ts,
               payload = excluded.payload",
        )
        .bind(merged.connector_id.to_string())
        .bind(&merged.chat_id)
        .bind(merged.last_message_ts)
        .bind(serde_json::to_string(&merged)?)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(observed)
    }

    async fn mark_observed_deleted(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        native_id: &str,
    ) -> StorageResult<Option<ObservedMessage>> {
        update_observed(&self.pool, connector_id, chat_id, native_id, |observed| {
            observed.deleted = true;
        })
        .await
    }

    async fn mark_observed_edited(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        native_id: &str,
        text: Option<String>,
    ) -> StorageResult<Option<ObservedMessage>> {
        update_observed(&self.pool, connector_id, chat_id, native_id, |observed| {
            observed.text = text;
            observed.edited = true;
        })
        .await
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
        let mut transaction = self.pool.begin().await?;

        // The chat id lives in the payload as well as the key columns, so rows
        // are re-encoded rather than moved with a bare UPDATE. `OR IGNORE`
        // leaves a row already recorded under the destination alone: that is
        // the form the chat is known by now.
        let rows = sqlx::query(
            "SELECT payload FROM observed_messages WHERE connector_id = ? AND chat_id = ?",
        )
        .bind(connector_id.to_string())
        .bind(from_chat_id)
        .fetch_all(&mut *transaction)
        .await?;
        for row in rows {
            let mut observed: ObservedMessage = serde_json::from_str(row.get("payload"))?;
            observed.chat_id = to_chat_id.to_owned();
            sqlx::query(
                "INSERT OR IGNORE INTO observed_messages
                   (connector_id, chat_id, platform_message_id, timestamp_ms, payload)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(connector_id.to_string())
            .bind(to_chat_id)
            .bind(&observed.native_id)
            .bind(observed.timestamp_ms)
            .bind(serde_json::to_string(&observed)?)
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query("DELETE FROM observed_messages WHERE connector_id = ? AND chat_id = ?")
            .bind(connector_id.to_string())
            .bind(from_chat_id)
            .execute(&mut *transaction)
            .await?;

        let summary = sqlx::query(
            "SELECT payload FROM observed_chats WHERE connector_id = ? AND chat_id = ?",
        )
        .bind(connector_id.to_string())
        .bind(from_chat_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = summary {
            let mut moved: ObservedChat = serde_json::from_str(row.get("payload"))?;
            moved.chat_id = to_chat_id.to_owned();
            let existing = sqlx::query(
                "SELECT payload FROM observed_chats WHERE connector_id = ? AND chat_id = ?",
            )
            .bind(connector_id.to_string())
            .bind(to_chat_id)
            .fetch_optional(&mut *transaction)
            .await?;
            // Both forms have a summary: keep the more recent activity, and keep
            // whichever title is known.
            if let Some(existing) = existing {
                let existing: ObservedChat = serde_json::from_str(existing.get("payload"))?;
                if existing.last_message_ts >= moved.last_message_ts {
                    moved.last_message_ts = existing.last_message_ts;
                    moved.last_message_preview = existing.last_message_preview;
                }
                moved.title = existing.title.or(moved.title);
            }
            sqlx::query(
                "INSERT INTO observed_chats (connector_id, chat_id, last_message_ts, payload)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(connector_id, chat_id) DO UPDATE SET
                   last_message_ts = excluded.last_message_ts,
                   payload = excluded.payload",
            )
            .bind(connector_id.to_string())
            .bind(to_chat_id)
            .bind(moved.last_message_ts)
            .bind(serde_json::to_string(&moved)?)
            .execute(&mut *transaction)
            .await?;
            sqlx::query("DELETE FROM observed_chats WHERE connector_id = ? AND chat_id = ?")
                .bind(connector_id.to_string())
                .bind(from_chat_id)
                .execute(&mut *transaction)
                .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    async fn list_observed_chats(&self) -> StorageResult<Vec<ObservedChat>> {
        let rows = sqlx::query("SELECT payload FROM observed_chats ORDER BY last_message_ts DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| serde_json::from_str(row.get("payload")).map_err(Into::into))
            .collect()
    }

    async fn observed_chat(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
    ) -> StorageResult<Option<ObservedChat>> {
        let row = sqlx::query(
            "SELECT payload FROM observed_chats WHERE connector_id = ? AND chat_id = ?",
        )
        .bind(connector_id.to_string())
        .bind(chat_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| serde_json::from_str(row.get("payload")).map_err(Into::into))
            .transpose()
    }

    async fn observed_messages(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        before_ms: Option<i64>,
        limit: u32,
    ) -> StorageResult<Vec<ObservedMessage>> {
        let rows = sqlx::query(
            "SELECT payload FROM observed_messages
             WHERE connector_id = ? AND chat_id = ? AND (? IS NULL OR timestamp_ms < ?)
             ORDER BY timestamp_ms DESC
             LIMIT ?",
        )
        .bind(connector_id.to_string())
        .bind(chat_id)
        .bind(before_ms)
        .bind(before_ms)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| serde_json::from_str(row.get("payload")).map_err(Into::into))
            .collect()
    }
}

#[async_trait]
impl SettingsOverrides for SqliteStorage {
    async fn settings_overrides(&self) -> StorageResult<Vec<StoredSetting>> {
        let rows = sqlx::query("SELECT key, value, updated_by FROM settings ORDER BY key")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| StoredSetting {
                key: row.get("key"),
                value: row.get("value"),
                updated_by: row.get("updated_by"),
            })
            .collect())
    }

    async fn write_settings_overrides(
        &self,
        deleted: &[String],
        upserted: Option<&StoredSetting>,
        updated_at_ms: i64,
    ) -> StorageResult<()> {
        let mut transaction = self.pool.begin().await?;
        for key in deleted {
            sqlx::query("DELETE FROM settings WHERE key = ?")
                .bind(key)
                .execute(&mut *transaction)
                .await?;
        }
        if let Some(setting) = upserted {
            sqlx::query(
                "INSERT INTO settings (key, value, updated_by, updated_at_ms) VALUES (?, ?, ?, ?)
                 ON CONFLICT(key) DO UPDATE SET
                   value = excluded.value, updated_by = excluded.updated_by,
                   updated_at_ms = excluded.updated_at_ms",
            )
            .bind(&setting.key)
            .bind(&setting.value)
            .bind(&setting.updated_by)
            .bind(updated_at_ms)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl PanelTokens for SqliteStorage {
    async fn put_panel_token(
        &self,
        key: &str,
        action: &str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> StorageResult<bool> {
        // One statement: insert, or replace a row holding the same action or
        // an expired one. A live row with another action is left alone, and
        // no row is written.
        let result = sqlx::query(
            "INSERT INTO panel_tokens (key, action, expires_at) VALUES (?1, ?2, ?4)
             ON CONFLICT(key) DO UPDATE SET
               action = excluded.action, expires_at = excluded.expires_at
             WHERE panel_tokens.action = excluded.action OR panel_tokens.expires_at < ?3",
        )
        .bind(key)
        .bind(action)
        .bind(now_ms)
        .bind(expires_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn panel_token(&self, key: &str, now_ms: i64) -> StorageResult<Option<String>> {
        let row = sqlx::query("SELECT action FROM panel_tokens WHERE key = ? AND expires_at >= ?")
            .bind(key)
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| row.get("action")))
    }

    async fn purge_panel_tokens(&self, now_ms: i64) -> StorageResult<u64> {
        let result = sqlx::query("DELETE FROM panel_tokens WHERE expires_at < ?")
            .bind(now_ms)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[async_trait]
impl SettingsDeclarations for SqliteStorage {
    async fn put_settings_declaration(
        &self,
        declaration: &SettingsDeclaration,
        updated_at_ms: i64,
    ) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO connector_settings_declarations
               (connector_id, descriptor, json_schema, base_json, updated_at_ms)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(connector_id) DO UPDATE SET
               descriptor = excluded.descriptor, json_schema = excluded.json_schema,
               base_json = excluded.base_json, updated_at_ms = excluded.updated_at_ms",
        )
        .bind(&declaration.connector_id.0)
        .bind(&declaration.descriptor)
        .bind(&declaration.json_schema)
        .bind(&declaration.base_json)
        .bind(updated_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn settings_declarations(&self) -> StorageResult<Vec<SettingsDeclaration>> {
        let rows = sqlx::query(
            "SELECT connector_id, descriptor, json_schema, base_json
             FROM connector_settings_declarations ORDER BY connector_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| SettingsDeclaration {
                connector_id: ConnectorId(row.get("connector_id")),
                descriptor: row.get("descriptor"),
                json_schema: row.get("json_schema"),
                base_json: row.get("base_json"),
            })
            .collect())
    }
}

/// Load one observed message, apply a mutation, and persist it, returning the
/// updated row. A `None` result means the message was never observed. Used by the
/// edit/delete markers so their read-modify-write stays atomic in one place.
async fn update_observed(
    pool: &SqlitePool,
    connector_id: &ConnectorId,
    chat_id: &str,
    native_id: &str,
    mutate: impl FnOnce(&mut ObservedMessage),
) -> StorageResult<Option<ObservedMessage>> {
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        "SELECT payload FROM observed_messages
         WHERE connector_id = ? AND chat_id = ? AND platform_message_id = ?",
    )
    .bind(connector_id.to_string())
    .bind(chat_id)
    .bind(native_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = row else {
        transaction.commit().await?;
        return Ok(None);
    };
    let mut observed: ObservedMessage = serde_json::from_str(row.get("payload"))?;
    mutate(&mut observed);
    sqlx::query(
        "UPDATE observed_messages SET payload = ?
         WHERE connector_id = ? AND chat_id = ? AND platform_message_id = ?",
    )
    .bind(serde_json::to_string(&observed)?)
    .bind(connector_id.to_string())
    .bind(chat_id)
    .bind(native_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Some(observed))
}

fn thread_key(address: &ChatAddress) -> String {
    address.thread_id.clone().unwrap_or_default()
}

fn parse_endpoint_id(value: &str) -> StorageResult<EndpointId> {
    uuid::Uuid::parse_str(value)
        .map(EndpointId)
        .map_err(|err| StorageError::Identifier(err.to_string()))
}

async fn put_sqlite_copy(pool: &SqlitePool, copy: MessageCopy, sent: bool) -> StorageResult<()> {
    sqlx::query(
        "INSERT INTO message_copies (message_id, endpoint_id, platform_message_id, sent, payload)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(message_id, endpoint_id) DO UPDATE SET
           platform_message_id = excluded.platform_message_id,
           sent = MAX(message_copies.sent, excluded.sent),
           payload = excluded.payload",
    )
    .bind(copy.message_id.to_string())
    .bind(copy.endpoint_id.to_string())
    .bind(&copy.native_id)
    .bind(i64::from(sent))
    .bind(serde_json::to_string(&copy)?)
    .execute(pool)
    .await?;
    Ok(())
}

#[async_trait]
impl ChatProfileStore for SqliteStorage {
    async fn chat_profile(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
    ) -> StorageResult<Option<ChatProfile>> {
        let row =
            sqlx::query("SELECT payload FROM chat_profiles WHERE connector_id = ? AND chat_id = ?")
                .bind(&connector_id.0)
                .bind(chat_id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|row| serde_json::from_str(row.get("payload")))
            .transpose()
            .map_err(Into::into)
    }

    async fn put_chat_profile(&self, profile: &ChatProfile) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO chat_profiles (connector_id, chat_id, payload) VALUES (?, ?, ?)
             ON CONFLICT(connector_id, chat_id) DO UPDATE SET payload = excluded.payload",
        )
        .bind(&profile.connector_id.0)
        .bind(&profile.chat_id)
        .bind(serde_json::to_string(profile)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// `table` and `key_column` are `'static` so that only names written in this
/// file can reach the SQL text; the values travel as bound parameters.
async fn upsert_json<T: serde::Serialize>(
    pool: &SqlitePool,
    table: &'static str,
    key_column: &'static str,
    key: String,
    value: &T,
) -> StorageResult<()> {
    let sql = format!(
        "INSERT INTO {table} ({key_column}, payload) VALUES (?, ?)
         ON CONFLICT({key_column}) DO UPDATE SET payload = excluded.payload"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(key)
        .bind(serde_json::to_string(value)?)
        .execute(pool)
        .await?;
    Ok(())
}

/// As [`upsert_json`]: only `'static` names reach the SQL text.
async fn get_json<T: serde::de::DeserializeOwned>(
    pool: &SqlitePool,
    table: &'static str,
    key_column: &'static str,
    key: String,
) -> StorageResult<Option<T>> {
    let sql = format!("SELECT payload FROM {table} WHERE {key_column} = ?");
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(key)
        .fetch_optional(pool)
        .await?;
    row.map(|row| serde_json::from_str(row.get("payload")))
        .transpose()
        .map_err(Into::into)
}

async fn insert_dispatch(pool: &SqlitePool, dispatch: &Dispatch) -> StorageResult<()> {
    sqlx::query(
        "INSERT INTO dispatches (id, target_endpoint, message_id, waiting_for, state, payload)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(dispatch.id.to_string())
    .bind(dispatch.to.endpoint_id.to_string())
    .bind(dispatch.message_id().map(|id| id.to_string()))
    .bind(waiting_for(dispatch).map(|id| id.to_string()))
    .bind(state_name(&dispatch.state))
    .bind(serde_json::to_string(dispatch)?)
    .execute(pool)
    .await?;
    Ok(())
}

fn waiting_for(dispatch: &Dispatch) -> Option<MessageId> {
    match dispatch.state {
        DispatchState::Blocked { waiting_for } => Some(waiting_for),
        _ => None,
    }
}

fn state_name(state: &DispatchState) -> &'static str {
    match state {
        DispatchState::Pending => "pending",
        DispatchState::Blocked { .. } => "blocked",
        DispatchState::Acknowledged { .. } => "acknowledged",
        DispatchState::Failed { .. } => "failed",
    }
}

type Transaction<'a> = sqlx::Transaction<'a, sqlx::Sqlite>;

/// Read a conversation inside a transaction, for a read-modify-write that must
/// not race another writer.
async fn conversation_in(
    transaction: &mut Transaction<'_>,
    conversation_id: ConversationId,
) -> StorageResult<Conversation> {
    let row = sqlx::query("SELECT payload FROM conversations WHERE id = ?")
        .bind(conversation_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(StorageError::ConversationNotFound(conversation_id))?;
    Ok(serde_json::from_str(row.get("payload"))?)
}

/// Rewrite a conversation's payload only. Its identity columns never change
/// after creation.
async fn update_conversation_payload(
    transaction: &mut Transaction<'_>,
    conversation: &Conversation,
) -> StorageResult<()> {
    sqlx::query("UPDATE conversations SET payload = ? WHERE id = ?")
        .bind(serde_json::to_string(conversation)?)
        .bind(conversation.id.to_string())
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Index an endpoint under its conversation. Idempotent.
async fn insert_conversation_endpoint(
    transaction: &mut Transaction<'_>,
    conversation_id: ConversationId,
    endpoint_id: EndpointId,
) -> StorageResult<()> {
    sqlx::query(
        "INSERT INTO conversation_endpoints (endpoint_id, conversation_id) VALUES (?, ?)
         ON CONFLICT(endpoint_id) DO UPDATE SET conversation_id = excluded.conversation_id",
    )
    .bind(endpoint_id.to_string())
    .bind(conversation_id.to_string())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}
