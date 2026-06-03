//! SQLite-backed [`Storage`]. Rows hold JSON-encoded model values keyed by id;
//! dispatch state is denormalized into columns so pending work can be queried.

use crate::shared::{bind_native_parent, dispatch_state};
use crate::{Storage, StorageError, StorageResult};
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use wuphf_model::{
    Alias, ChatSettings, ConnectorId, Contact, Conversation, ConversationId, DedupKey,
    DispatchCommand, DispatchId, DispatchState, Endpoint, EndpointId, MediaBlob, MediaId, Message,
    MessageCopy, MessageId, PlatformAddress, ReplicaSet, ReplicaSetId, StoredDispatch,
    ThreadMapping,
};

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
        sqlx::migrate!("../../migrations").run(&pool).await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Storage for SqliteStorage {
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

    async fn put_replica_set(&self, replica_set: ReplicaSet) -> StorageResult<()> {
        upsert_json(
            &self.pool,
            "replica_sets",
            "id",
            replica_set.id.to_string(),
            &replica_set,
        )
        .await
    }

    async fn replica_set(&self, replica_set_id: ReplicaSetId) -> StorageResult<ReplicaSet> {
        get_json(&self.pool, "replica_sets", "id", replica_set_id.to_string())
            .await?
            .ok_or(StorageError::ReplicaSetNotFound(replica_set_id))
    }

    async fn replica_set_for_endpoint(
        &self,
        endpoint_id: EndpointId,
    ) -> StorageResult<Option<ReplicaSetId>> {
        let needle = endpoint_id.to_string();
        let rows = sqlx::query("SELECT payload FROM replica_sets")
            .fetch_all(&self.pool)
            .await?;
        for row in rows {
            let replica_set: ReplicaSet = serde_json::from_str(row.get("payload"))?;
            if replica_set
                .endpoints
                .iter()
                .any(|id| id.to_string() == needle)
            {
                return Ok(Some(replica_set.id));
            }
        }
        Ok(None)
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
        address: &PlatformAddress,
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

    async fn put_conversation(&self, conversation: Conversation) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO conversations
               (id, rule_id, replica_set_id, source_connector, source_chat_id, source_thread_key, payload)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
        )
        .bind(conversation.id.to_string())
        .bind(conversation.rule_id.to_string())
        .bind(conversation.replica_set_id.to_string())
        .bind(conversation.source_connector.to_string())
        .bind(&conversation.source_address.chat_id)
        .bind(thread_key(&conversation.source_address))
        .bind(serde_json::to_string(&conversation)?)
        .execute(&self.pool)
        .await?;
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

    async fn conversation_for_source(
        &self,
        rule_id: &wuphf_model::RuleId,
        connector_id: &ConnectorId,
        address: &PlatformAddress,
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

    async fn conversation_for_replica_set(
        &self,
        replica_set_id: ReplicaSetId,
    ) -> StorageResult<Option<Conversation>> {
        let row = sqlx::query("SELECT payload FROM conversations WHERE replica_set_id = ?")
            .bind(replica_set_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| serde_json::from_str(row.get("payload")))
            .transpose()
            .map_err(Into::into)
    }

    async fn put_thread_mapping(&self, mapping: ThreadMapping) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO thread_mappings (conversation_id, connector_id, container, thread_id)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(conversation_id, connector_id) DO UPDATE SET
               container = excluded.container,
               thread_id = excluded.thread_id",
        )
        .bind(mapping.conversation_id.to_string())
        .bind(mapping.connector_id.to_string())
        .bind(&mapping.container)
        .bind(&mapping.thread_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn thread_for_conversation(
        &self,
        conversation_id: ConversationId,
        connector_id: &ConnectorId,
    ) -> StorageResult<Option<String>> {
        let row = sqlx::query(
            "SELECT thread_id FROM thread_mappings WHERE conversation_id = ? AND connector_id = ?",
        )
        .bind(conversation_id.to_string())
        .bind(connector_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get::<String, _>("thread_id")))
    }

    async fn conversation_for_thread(
        &self,
        connector_id: &ConnectorId,
        container: &str,
        thread_id: &str,
    ) -> StorageResult<Option<ConversationId>> {
        let row = sqlx::query(
            "SELECT conversation_id FROM thread_mappings
             WHERE connector_id = ? AND container = ? AND thread_id = ?",
        )
        .bind(connector_id.to_string())
        .bind(container)
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            uuid::Uuid::parse_str(row.get("conversation_id"))
                .map(ConversationId)
                .map_err(|err| StorageError::Identifier(err.to_string()))
        })
        .transpose()
    }

    async fn claim_event(&self, key: DedupKey) -> StorageResult<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO seen_events (platform, chat_id, platform_message_id) VALUES (?, ?, ?)",
        )
        .bind(serde_json::to_string(&key.platform)?)
        .bind(key.chat_id)
        .bind(key.platform_message_id)
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

    async fn is_sent_copy(
        &self,
        endpoint_id: EndpointId,
        platform_message_id: &str,
    ) -> StorageResult<bool> {
        let row = sqlx::query(
            "SELECT sent FROM message_copies WHERE endpoint_id = ? AND platform_message_id = ?",
        )
        .bind(endpoint_id.to_string())
        .bind(platform_message_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some_and(|row| row.get::<i64, _>("sent") != 0))
    }

    async fn message_id_for_copy(
        &self,
        endpoint_id: EndpointId,
        platform_message_id: &str,
    ) -> StorageResult<Option<MessageId>> {
        let row = sqlx::query(
            "SELECT payload FROM message_copies WHERE endpoint_id = ? AND platform_message_id = ?",
        )
        .bind(endpoint_id.to_string())
        .bind(platform_message_id)
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

    async fn put_contact(&self, contact: Contact) -> StorageResult<()> {
        upsert_json(&self.pool, "contacts", "id", contact.id.clone(), &contact).await
    }

    async fn put_chat_settings(&self, settings: ChatSettings) -> StorageResult<()> {
        upsert_json(
            &self.pool,
            "chat_settings",
            "endpoint_id",
            settings.endpoint_id.to_string(),
            &settings,
        )
        .await
    }

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
               )",
        )
        .bind(cutoff_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
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
        insert_dispatch(&self.pool, &dispatch).await?;
        Ok(dispatch)
    }

    async fn pending_dispatches(&self) -> StorageResult<Vec<StoredDispatch>> {
        let rows =
            sqlx::query("SELECT payload FROM dispatches WHERE state = 'pending' ORDER BY rowid")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|row| serde_json::from_str(row.get("payload")).map_err(Into::into))
            .collect()
    }

    async fn dispatch(&self, dispatch_id: DispatchId) -> StorageResult<Option<StoredDispatch>> {
        get_json(&self.pool, "dispatches", "id", dispatch_id.to_string()).await
    }

    async fn acknowledge_dispatch(
        &self,
        dispatch_id: DispatchId,
        platform_message_id: String,
    ) -> StorageResult<Vec<StoredDispatch>> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT payload FROM dispatches WHERE id = ?")
            .bind(dispatch_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StorageError::DispatchNotFound(dispatch_id))?;
        let mut dispatch: StoredDispatch = serde_json::from_str(row.get("payload"))?;
        dispatch.state = DispatchState::Acknowledged {
            platform_message_id: platform_message_id.clone(),
        };
        sqlx::query("UPDATE dispatches SET waiting_for = ?, state = ?, payload = ? WHERE id = ?")
            .bind(None::<String>)
            .bind(state_name(&dispatch.state))
            .bind(serde_json::to_string(&dispatch)?)
            .bind(dispatch.id.to_string())
            .execute(&mut *transaction)
            .await?;
        // A retitle carries no logical message, so it records no copy and
        // releases no replies; acknowledging it just closes out the dispatch.
        let Some(message_id) = dispatch.command.message_id() else {
            transaction.commit().await?;
            return Ok(Vec::new());
        };
        let copy = MessageCopy {
            message_id,
            endpoint_id: dispatch.command.target_endpoint,
            platform_message_id: platform_message_id.clone(),
        };
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
        .bind(&copy.platform_message_id)
        .bind(serde_json::to_string(&copy)?)
        .execute(&mut *transaction)
        .await?;

        let rows = sqlx::query(
            "SELECT payload FROM dispatches WHERE state = 'blocked' AND waiting_for = ? AND target_endpoint = ?",
        )
        .bind(message_id.to_string())
        .bind(dispatch.command.target_endpoint.to_string())
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
            let mut blocked: StoredDispatch = serde_json::from_str(row.get("payload"))?;
            bind_native_parent(
                &mut blocked.command,
                parent.clone(),
                platform_message_id.clone(),
            );
            blocked.state = DispatchState::Pending;
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
        let mut dispatch: StoredDispatch = serde_json::from_str(row.get("payload"))?;
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

fn thread_key(address: &PlatformAddress) -> String {
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
    .bind(&copy.platform_message_id)
    .bind(i64::from(sent))
    .bind(serde_json::to_string(&copy)?)
    .execute(pool)
    .await?;
    Ok(())
}

async fn upsert_json<T: serde::Serialize>(
    pool: &SqlitePool,
    table: &str,
    key_column: &str,
    key: String,
    value: &T,
) -> StorageResult<()> {
    let sql = format!(
        "INSERT INTO {table} ({key_column}, payload) VALUES (?, ?)
         ON CONFLICT({key_column}) DO UPDATE SET payload = excluded.payload"
    );
    sqlx::query(&sql)
        .bind(key)
        .bind(serde_json::to_string(value)?)
        .execute(pool)
        .await?;
    Ok(())
}

async fn get_json<T: serde::de::DeserializeOwned>(
    pool: &SqlitePool,
    table: &str,
    key_column: &str,
    key: String,
) -> StorageResult<Option<T>> {
    let sql = format!("SELECT payload FROM {table} WHERE {key_column} = ?");
    let row = sqlx::query(&sql).bind(key).fetch_optional(pool).await?;
    row.map(|row| serde_json::from_str(row.get("payload")))
        .transpose()
        .map_err(Into::into)
}

async fn insert_dispatch(pool: &SqlitePool, dispatch: &StoredDispatch) -> StorageResult<()> {
    sqlx::query(
        "INSERT INTO dispatches (id, target_endpoint, message_id, waiting_for, state, payload)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(dispatch.id.to_string())
    .bind(dispatch.command.target_endpoint.to_string())
    .bind(dispatch.command.message_id().map(|id| id.to_string()))
    .bind(waiting_for(dispatch).map(|id| id.to_string()))
    .bind(state_name(&dispatch.state))
    .bind(serde_json::to_string(dispatch)?)
    .execute(pool)
    .await?;
    Ok(())
}

fn waiting_for(dispatch: &StoredDispatch) -> Option<MessageId> {
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
