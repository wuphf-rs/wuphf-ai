use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use thiserror::Error;
use tokio::sync::RwLock;
use wuphf_model::{
    ChatSettings, Contact, DedupKey, DispatchCommand, DispatchId, DispatchPayload, DispatchState,
    Endpoint, EndpointId, GroupId, Message, MessageCopy, MessageId, ReplicationGroup,
    StoredDispatch,
};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("endpoint {0} is not configured")]
    EndpointNotFound(EndpointId),
    #[error("replication group {0} is not configured")]
    GroupNotFound(GroupId),
    #[error("dispatch {0} is not configured")]
    DispatchNotFound(DispatchId),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

pub type StorageResult<T> = Result<T, StorageError>;

#[async_trait]
pub trait Storage: Send + Sync + 'static {
    async fn put_endpoint(&self, endpoint: Endpoint) -> StorageResult<()>;
    async fn endpoint(&self, endpoint_id: EndpointId) -> StorageResult<Endpoint>;
    async fn put_group(&self, group: ReplicationGroup) -> StorageResult<()>;
    async fn group(&self, group_id: GroupId) -> StorageResult<ReplicationGroup>;
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
    async fn put_contact(&self, contact: Contact) -> StorageResult<()>;
    async fn put_chat_settings(&self, settings: ChatSettings) -> StorageResult<()>;
    async fn enqueue_dispatch(
        &self,
        command: DispatchCommand,
        waiting_for: Option<MessageId>,
    ) -> StorageResult<StoredDispatch>;
    async fn pending_dispatches(&self) -> StorageResult<Vec<StoredDispatch>>;
    async fn acknowledge_dispatch(
        &self,
        dispatch_id: DispatchId,
        platform_message_id: String,
    ) -> StorageResult<Vec<StoredDispatch>>;
}

#[derive(Default)]
pub struct InMemoryStorage {
    state: RwLock<State>,
}

#[derive(Default)]
struct State {
    endpoints: HashMap<EndpointId, Endpoint>,
    groups: HashMap<GroupId, ReplicationGroup>,
    messages: HashMap<MessageId, Message>,
    copies: HashMap<(MessageId, EndpointId), MessageCopy>,
    copy_ids: HashMap<(EndpointId, String), MessageId>,
    sent_copies: HashSet<(EndpointId, String)>,
    seen_events: HashSet<DedupKey>,
    contacts: HashMap<String, Contact>,
    chat_settings: HashMap<EndpointId, ChatSettings>,
    dispatches: HashMap<DispatchId, StoredDispatch>,
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

    async fn put_group(&self, group: ReplicationGroup) -> StorageResult<()> {
        self.state.write().await.groups.insert(group.id, group);
        Ok(())
    }

    async fn group(&self, group_id: GroupId) -> StorageResult<ReplicationGroup> {
        self.state
            .read()
            .await
            .groups
            .get(&group_id)
            .cloned()
            .ok_or(StorageError::GroupNotFound(group_id))
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
        let message_id = dispatch.command.message_id();
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

        let mut released = Vec::new();
        for candidate in state.dispatches.values_mut() {
            if candidate.command.target_endpoint == endpoint_id
                && candidate.state
                    == (DispatchState::Blocked {
                        waiting_for: message_id,
                    })
            {
                set_native_reply(&mut candidate.command, platform_message_id.clone());
                candidate.state = DispatchState::Pending;
                released.push(candidate.clone());
            }
        }
        Ok(released)
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

    async fn put_group(&self, group: ReplicationGroup) -> StorageResult<()> {
        upsert_json(
            &self.pool,
            "replication_groups",
            "id",
            group.id.to_string(),
            &group,
        )
        .await
    }

    async fn group(&self, group_id: GroupId) -> StorageResult<ReplicationGroup> {
        get_json(&self.pool, "replication_groups", "id", group_id.to_string())
            .await?
            .ok_or(StorageError::GroupNotFound(group_id))
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
        upsert_json(
            &self.pool,
            "messages",
            "id",
            message.id.to_string(),
            &message,
        )
        .await
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
        let copy = MessageCopy {
            message_id: dispatch.command.message_id(),
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
        .bind(dispatch.command.message_id().to_string())
        .bind(dispatch.command.target_endpoint.to_string())
        .fetch_all(&mut *transaction)
        .await?;
        let mut released = Vec::new();
        for row in rows {
            let mut blocked: StoredDispatch = serde_json::from_str(row.get("payload"))?;
            set_native_reply(&mut blocked.command, platform_message_id.clone());
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
}

fn dispatch_state(waiting_for: Option<MessageId>) -> DispatchState {
    match waiting_for {
        Some(waiting_for) => DispatchState::Blocked { waiting_for },
        None => DispatchState::Pending,
    }
}

fn set_native_reply(command: &mut DispatchCommand, platform_message_id: String) {
    match &mut command.payload {
        DispatchPayload::MessageCreate {
            reply_to_platform_message_id,
            ..
        } => *reply_to_platform_message_id = Some(platform_message_id),
    }
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
    .bind(dispatch.command.message_id().to_string())
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{DispatchPayload, Platform};

    #[tokio::test]
    async fn sqlite_persists_copy_state_and_releases_blocked_reply() {
        let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
        let url = format!("sqlite://{}", path.display());
        let parent_id = MessageId::new();
        let reply_id = MessageId::new();
        let endpoint_id = EndpointId::new();

        let storage = SqliteStorage::connect(&url).await.unwrap();
        storage
            .put_endpoint(Endpoint {
                id: endpoint_id,
                platform: Platform::Telegram,
                chat_id: "-100".into(),
                thread_id: Some("7".into()),
            })
            .await
            .unwrap();
        let parent = storage
            .enqueue_dispatch(command(parent_id, endpoint_id), None)
            .await
            .unwrap();
        storage
            .enqueue_dispatch(command(reply_id, endpoint_id), Some(parent_id))
            .await
            .unwrap();
        let released = storage
            .acknowledge_dispatch(parent.id, "telegram-parent".into())
            .await
            .unwrap();
        assert_eq!(released.len(), 1);
        let DispatchPayload::MessageCreate {
            reply_to_platform_message_id,
            ..
        } = &released[0].command.payload;
        assert_eq!(
            reply_to_platform_message_id.as_deref(),
            Some("telegram-parent")
        );
        drop(storage);

        let reopened = SqliteStorage::connect(&url).await.unwrap();
        assert_eq!(reopened.pending_dispatches().await.unwrap(), released);
        assert!(
            reopened
                .is_sent_copy(endpoint_id, "telegram-parent")
                .await
                .unwrap()
        );
    }

    fn command(message_id: MessageId, endpoint_id: EndpointId) -> DispatchCommand {
        DispatchCommand {
            target_endpoint: endpoint_id,
            payload: DispatchPayload::MessageCreate {
                message: Message {
                    id: message_id,
                    group_id: GroupId::new(),
                    sender: "test".into(),
                    participant: None,
                    text: Some("test".into()),
                    attachments: vec![],
                    reply_to: None,
                    timestamp_ms: 0,
                },
                reply_to_platform_message_id: None,
            },
        }
    }
}
