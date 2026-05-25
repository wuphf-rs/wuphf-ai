use crate::{config::Config, store::Store};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;
use wuphf_proto::v1::{AttachmentDescriptor, CreateMessage, DispatchCreateMessage};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("connector is not authorized for endpoint '{0}'")]
    UnauthorizedEndpoint(String),
    #[error("reply target '{0}' does not identify a known message copy at the origin endpoint")]
    UnknownReply(String),
    #[error("dispatch does not belong to the authenticated connector")]
    UnauthorizedDispatch,
    #[error("dispatch acknowledgement conflicts with its recorded platform message ID")]
    ConflictingAcknowledgement,
    #[error("persistent state error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("stored attachment metadata cannot be decoded: {0}")]
    Metadata(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct Engine {
    pub config: Arc<Config>,
    store: Store,
}

#[derive(Debug)]
pub struct IngestResult {
    pub logical_message_id: String,
    pub target_connectors: BTreeSet<String>,
    pub was_new: bool,
}

#[derive(Serialize, Deserialize)]
struct StoredAttachment {
    kind: i32,
    filename: String,
    mime_type: String,
    size_bytes: u64,
}

impl Engine {
    pub fn new(config: Arc<Config>, store: Store) -> Self {
        Self { config, store }
    }

    pub async fn ingest(
        &self,
        connector_id: &str,
        message: CreateMessage,
    ) -> Result<IngestResult, EngineError> {
        if !self
            .config
            .connector_owns_endpoint(connector_id, &message.endpoint_id)
        {
            return Err(EngineError::UnauthorizedEndpoint(message.endpoint_id));
        }

        let targets = self.config.target_endpoints(&message.endpoint_id);
        let target_connectors = targets
            .iter()
            .map(|endpoint| endpoint.connector_id.clone())
            .collect();
        let mut tx = self.store.pool.begin().await?;

        if let Some(existing) = sqlx::query_scalar::<_, String>(
            "SELECT logical_message_id FROM ingested_events WHERE connector_id = ? AND event_id = ?",
        )
        .bind(connector_id)
        .bind(&message.event_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            tx.commit().await?;
            return Ok(IngestResult {
                logical_message_id: existing,
                target_connectors: BTreeSet::new(),
                was_new: false,
            });
        }

        if let Some(existing) = sqlx::query_scalar::<_, String>(
            "SELECT logical_message_id FROM message_copies WHERE endpoint_id = ? AND platform_message_id = ?",
        )
        .bind(&message.endpoint_id)
        .bind(&message.platform_message_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            sqlx::query(
                "INSERT INTO ingested_events(connector_id, event_id, logical_message_id) VALUES (?, ?, ?)",
            )
            .bind(connector_id)
            .bind(&message.event_id)
            .bind(&existing)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(IngestResult {
                logical_message_id: existing,
                target_connectors: BTreeSet::new(),
                was_new: false,
            });
        }

        let parent_id = if let Some(reply) = &message.reply_to_platform_message_id {
            Some(
                sqlx::query_scalar::<_, String>(
                    "SELECT logical_message_id FROM message_copies WHERE endpoint_id = ? AND platform_message_id = ?",
                )
                .bind(&message.endpoint_id)
                .bind(reply)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| EngineError::UnknownReply(reply.clone()))?,
            )
        } else {
            None
        };
        let logical_id = Uuid::new_v4().to_string();
        let attachments: Vec<_> = message
            .attachments
            .iter()
            .map(|item| StoredAttachment {
                kind: item.kind,
                filename: item.filename.clone(),
                mime_type: item.mime_type.clone(),
                size_bytes: item.size_bytes,
            })
            .collect();

        sqlx::query(
            "INSERT INTO logical_messages
             (id, origin_endpoint_id, parent_logical_message_id, sender_id, sender_display_name, sent_at_unix_ms, text, attachments_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&logical_id)
        .bind(&message.endpoint_id)
        .bind(&parent_id)
        .bind(&message.sender_id)
        .bind(&message.sender_display_name)
        .bind(message.sent_at_unix_ms)
        .bind(&message.text)
        .bind(serde_json::to_string(&attachments)?)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO message_copies(id, logical_message_id, endpoint_id, platform_message_id, source)
             VALUES (?, ?, ?, ?, 'origin')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&logical_id)
        .bind(&message.endpoint_id)
        .bind(&message.platform_message_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO ingested_events(connector_id, event_id, logical_message_id) VALUES (?, ?, ?)",
        )
        .bind(connector_id)
        .bind(&message.event_id)
        .bind(&logical_id)
        .execute(&mut *tx)
        .await?;

        for endpoint in targets {
            let sequence = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(endpoint_sequence), 0) + 1 FROM dispatch_commands WHERE target_endpoint_id = ?",
            )
            .bind(&endpoint.id)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO dispatch_commands
                 (id, logical_message_id, target_endpoint_id, target_connector_id, parent_logical_message_id, endpoint_sequence, status)
                 VALUES (?, ?, ?, ?, ?, ?, 'pending')",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&logical_id)
            .bind(&endpoint.id)
            .bind(&endpoint.connector_id)
            .bind(&parent_id)
            .bind(sequence)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(IngestResult {
            logical_message_id: logical_id,
            target_connectors,
            was_new: true,
        })
    }

    pub async fn claim_dispatches(
        &self,
        connector_id: &str,
        allow_early_retry: bool,
    ) -> Result<Vec<DispatchCreateMessage>, EngineError> {
        let now = now_unix_ms();
        let lease_until = now.saturating_add(
            i64::try_from(self.config.delivery.acknowledgement_timeout_ms).unwrap_or(i64::MAX),
        );
        let mut tx = self.store.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT d.id, d.logical_message_id, d.target_endpoint_id, l.text, l.sender_id,
                    l.sender_display_name, l.sent_at_unix_ms, l.attachments_json,
                    parent_copy.platform_message_id AS reply_platform_message_id
             FROM dispatch_commands d
             JOIN logical_messages l ON l.id = d.logical_message_id
             LEFT JOIN message_copies parent_copy
               ON parent_copy.logical_message_id = d.parent_logical_message_id
              AND parent_copy.endpoint_id = d.target_endpoint_id
             WHERE d.target_connector_id = ?
               AND d.status IN ('pending', 'failed')
               AND d.terminal_failure = 0
               AND (d.attempt_count = 0 OR ? OR d.next_attempt_at_unix_ms <= ?)
               AND (d.parent_logical_message_id IS NULL OR parent_copy.id IS NOT NULL)
               AND NOT EXISTS (
                   SELECT 1 FROM dispatch_commands earlier
                   WHERE earlier.target_endpoint_id = d.target_endpoint_id
                     AND earlier.endpoint_sequence < d.endpoint_sequence
                     AND earlier.status != 'completed'
               )
             ORDER BY d.target_endpoint_id, d.endpoint_sequence",
        )
        .bind(connector_id)
        .bind(allow_early_retry)
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;

        let mut dispatches = Vec::with_capacity(rows.len());
        for row in rows {
            let dispatch_id: String = row.get("id");
            sqlx::query(
                "UPDATE dispatch_commands
                 SET status = 'pending',
                     attempt_count = attempt_count + 1,
                     last_attempted_at_unix_ms = ?,
                     next_attempt_at_unix_ms = ?
                 WHERE id = ?",
            )
            .bind(now)
            .bind(lease_until)
            .bind(&dispatch_id)
            .execute(&mut *tx)
            .await?;
            let metadata: Vec<StoredAttachment> =
                serde_json::from_str(row.get::<&str, _>("attachments_json"))?;
            dispatches.push(DispatchCreateMessage {
                dispatch_id,
                logical_message_id: row.get("logical_message_id"),
                endpoint_id: row.get("target_endpoint_id"),
                text: row.get("text"),
                sender_id: row.get("sender_id"),
                sender_display_name: row.get("sender_display_name"),
                sent_at_unix_ms: row.get("sent_at_unix_ms"),
                reply_to_platform_message_id: row.get("reply_platform_message_id"),
                attachments: metadata
                    .into_iter()
                    .map(|item| AttachmentDescriptor {
                        kind: item.kind,
                        filename: item.filename,
                        mime_type: item.mime_type,
                        size_bytes: item.size_bytes,
                    })
                    .collect(),
            });
        }
        tx.commit().await?;
        Ok(dispatches)
    }

    pub async fn acknowledge(
        &self,
        connector_id: &str,
        dispatch_id: &str,
        platform_message_id: &str,
    ) -> Result<(), EngineError> {
        let mut tx = self.store.pool.begin().await?;
        let row = sqlx::query(
            "SELECT logical_message_id, target_endpoint_id, status, acknowledged_platform_message_id
             FROM dispatch_commands WHERE id = ? AND target_connector_id = ?",
        )
        .bind(dispatch_id)
        .bind(connector_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(EngineError::UnauthorizedDispatch)?;
        let status: String = row.get("status");
        let previous: Option<String> = row.get("acknowledged_platform_message_id");
        if status == "completed" {
            if previous.as_deref() == Some(platform_message_id) {
                tx.commit().await?;
                return Ok(());
            }
            return Err(EngineError::ConflictingAcknowledgement);
        }

        let logical_id: String = row.get("logical_message_id");
        let endpoint_id: String = row.get("target_endpoint_id");
        sqlx::query(
            "INSERT INTO message_copies
             (id, logical_message_id, endpoint_id, platform_message_id, source, dispatch_id)
             VALUES (?, ?, ?, ?, 'dispatch', ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&logical_id)
        .bind(&endpoint_id)
        .bind(platform_message_id)
        .bind(dispatch_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE dispatch_commands SET status = 'completed', acknowledged_platform_message_id = ?,
             acknowledged_at = CURRENT_TIMESTAMP, last_error = NULL WHERE id = ?",
        )
        .bind(platform_message_id)
        .bind(dispatch_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail(
        &self,
        connector_id: &str,
        dispatch_id: &str,
        reason: &str,
        permanent: bool,
    ) -> Result<Option<Duration>, EngineError> {
        let mut tx = self.store.pool.begin().await?;
        let row = sqlx::query(
            "SELECT attempt_count, status, terminal_failure, next_attempt_at_unix_ms
             FROM dispatch_commands
             WHERE id = ? AND target_connector_id = ? AND status != 'completed'",
        )
        .bind(dispatch_id)
        .bind(connector_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(EngineError::UnauthorizedDispatch)?;
        let attempts: i64 = row.get("attempt_count");
        let status: String = row.get("status");
        let terminal_failure: bool = row.get("terminal_failure");
        if terminal_failure {
            tx.commit().await?;
            return Ok(None);
        }
        if status == "failed" && !permanent {
            let retry_at: Option<i64> = row.get("next_attempt_at_unix_ms");
            tx.commit().await?;
            return Ok(retry_at.map(|retry_at| {
                Duration::from_millis(
                    retry_at
                        .saturating_sub(now_unix_ms())
                        .try_into()
                        .unwrap_or_default(),
                )
            }));
        }
        let retry_delay = if permanent {
            None
        } else {
            Some(self.retry_delay(attempts))
        };
        let retry_at = retry_delay.map(|delay| {
            now_unix_ms().saturating_add(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX))
        });
        sqlx::query(
            "UPDATE dispatch_commands
             SET status = 'failed', last_error = ?, terminal_failure = ?,
                 next_attempt_at_unix_ms = ?
             WHERE id = ?",
        )
        .bind(reason)
        .bind(permanent)
        .bind(retry_at)
        .bind(dispatch_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(retry_delay)
    }

    fn retry_delay(&self, attempts: i64) -> Duration {
        let exponent = u32::try_from(attempts.saturating_sub(1))
            .unwrap_or(u32::MAX)
            .min(63);
        let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let delay = self
            .config
            .delivery
            .retry_initial_ms
            .saturating_mul(multiplier)
            .min(self.config.delivery.retry_max_ms);
        Duration::from_millis(delay)
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn setup(groups: &str) -> (Engine, TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.sqlite");
        let config: Config = toml::from_str(&format!(
            r#"
                [server]
                listen = "127.0.0.1:1"
                [database]
                url = "sqlite://{}"
                [[connectors]]
                id = "a"
                token = "a-token"
                [[connectors]]
                id = "b"
                token = "b-token"
                [[connectors]]
                id = "c"
                token = "c-token"
                [[endpoints]]
                id = "a-room"
                connector_id = "a"
                address = "a"
                [[endpoints]]
                id = "b-room"
                connector_id = "b"
                address = "b"
                [[endpoints]]
                id = "c-room"
                connector_id = "c"
                address = "c"
                {}
            "#,
            path.display(),
            groups
        ))
        .unwrap();
        config.validate().unwrap();
        let config = Arc::new(config);
        let store = Store::open(&config).await.unwrap();
        (Engine::new(config, store), temp)
    }

    fn create(
        event: &str,
        endpoint: &str,
        platform_id: &str,
        reply: Option<&str>,
    ) -> CreateMessage {
        CreateMessage {
            event_id: event.into(),
            endpoint_id: endpoint.into(),
            platform_message_id: platform_id.into(),
            sender_id: "sender".into(),
            sender_display_name: "Sender".into(),
            sent_at_unix_ms: 10,
            text: event.into(),
            reply_to_platform_message_id: reply.map(str::to_string),
            attachments: Vec::new(),
        }
    }

    #[tokio::test]
    async fn overlapping_groups_fan_out_once_and_exclude_origin() {
        let (engine, _temp) = setup(
            r#"
                [[replication_groups]]
                id = "one"
                endpoints = ["a-room", "b-room", "c-room"]
                [[replication_groups]]
                id = "two"
                endpoints = ["a-room", "b-room"]
            "#,
        )
        .await;
        engine
            .ingest("a", create("one", "a-room", "a-one", None))
            .await
            .unwrap();

        assert_eq!(engine.claim_dispatches("a", false).await.unwrap().len(), 0);
        assert_eq!(engine.claim_dispatches("b", false).await.unwrap().len(), 1);
        assert_eq!(engine.claim_dispatches("c", false).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deduplicates_events_and_prevents_dispatched_copy_reentry() {
        let (engine, _temp) = setup(
            r#"
                [[replication_groups]]
                id = "one"
                endpoints = ["a-room", "b-room"]
            "#,
        )
        .await;
        let first = engine
            .ingest("a", create("event", "a-room", "origin", None))
            .await
            .unwrap();
        let duplicate = engine
            .ingest("a", create("event", "a-room", "origin", None))
            .await
            .unwrap();
        assert!(!duplicate.was_new);
        assert_eq!(first.logical_message_id, duplicate.logical_message_id);

        let dispatch = engine.claim_dispatches("b", false).await.unwrap().remove(0);
        engine
            .acknowledge("b", &dispatch.dispatch_id, "b-created")
            .await
            .unwrap();
        let observed = engine
            .ingest("b", create("observed", "b-room", "b-created", None))
            .await
            .unwrap();
        assert!(!observed.was_new);
        assert_eq!(engine.claim_dispatches("a", false).await.unwrap().len(), 0);

        let logical_messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM logical_messages")
            .fetch_one(&engine.store.pool)
            .await
            .unwrap();
        assert_eq!(logical_messages, 1);
    }

    #[tokio::test]
    async fn reply_waits_for_target_parent_copy_and_uses_native_parent_id() {
        let (engine, _temp) = setup(
            r#"
                [[replication_groups]]
                id = "one"
                endpoints = ["a-room", "b-room"]
            "#,
        )
        .await;
        engine
            .ingest("a", create("parent", "a-room", "a-parent", None))
            .await
            .unwrap();
        engine
            .ingest("a", create("reply", "a-room", "a-reply", Some("a-parent")))
            .await
            .unwrap();
        let first = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text, "parent");

        engine
            .acknowledge("b", &first[0].dispatch_id, "b-parent")
            .await
            .unwrap();
        engine
            .acknowledge("b", &first[0].dispatch_id, "b-parent")
            .await
            .unwrap();
        let reply = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(reply.len(), 1);
        assert_eq!(
            reply[0].reply_to_platform_message_id.as_deref(),
            Some("b-parent")
        );
    }

    #[tokio::test]
    async fn pending_dispatch_survives_store_reopen() {
        let (engine, _temp) = setup(
            r#"
                [[replication_groups]]
                id = "one"
                endpoints = ["a-room", "b-room"]
            "#,
        )
        .await;
        engine
            .ingest("a", create("pending", "a-room", "origin", None))
            .await
            .unwrap();
        let reopened_store = Store::open(&engine.config).await.unwrap();
        let reopened = Engine::new(engine.config.clone(), reopened_store);
        assert_eq!(reopened.claim_dispatches("b", true).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn records_attempts_and_does_not_duplicate_an_in_flight_dispatch() {
        let (engine, _temp) = setup(
            r#"
                [[replication_groups]]
                id = "one"
                endpoints = ["a-room", "b-room"]
            "#,
        )
        .await;
        engine
            .ingest("a", create("pending", "a-room", "origin", None))
            .await
            .unwrap();

        let dispatch = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(dispatch.len(), 1);
        assert!(engine
            .claim_dispatches("b", false)
            .await
            .unwrap()
            .is_empty());
        let attempts: i64 = sqlx::query_scalar("SELECT attempt_count FROM dispatch_commands")
            .fetch_one(&engine.store.pool)
            .await
            .unwrap();
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn applies_retry_backoff_and_stops_terminal_failures() {
        let (engine, _temp) = setup(
            r#"
                [[replication_groups]]
                id = "one"
                endpoints = ["a-room", "b-room"]
            "#,
        )
        .await;
        engine
            .ingest("a", create("retry", "a-room", "origin", None))
            .await
            .unwrap();
        let first = engine.claim_dispatches("b", false).await.unwrap().remove(0);
        assert_eq!(
            engine
                .fail("b", &first.dispatch_id, "transient", false)
                .await
                .unwrap(),
            Some(Duration::from_millis(1_000))
        );
        let original_retry_at: i64 =
            sqlx::query_scalar("SELECT next_attempt_at_unix_ms FROM dispatch_commands")
                .fetch_one(&engine.store.pool)
                .await
                .unwrap();
        engine
            .fail("b", &first.dispatch_id, "duplicate transient", false)
            .await
            .unwrap();
        let preserved_retry_at: i64 =
            sqlx::query_scalar("SELECT next_attempt_at_unix_ms FROM dispatch_commands")
                .fetch_one(&engine.store.pool)
                .await
                .unwrap();
        assert_eq!(preserved_retry_at, original_retry_at);
        assert!(engine
            .claim_dispatches("b", false)
            .await
            .unwrap()
            .is_empty());
        let retried = engine.claim_dispatches("b", true).await.unwrap().remove(0);
        assert_eq!(
            engine
                .fail("b", &retried.dispatch_id, "transient again", false)
                .await
                .unwrap(),
            Some(Duration::from_millis(2_000))
        );
        let final_attempt = engine.claim_dispatches("b", true).await.unwrap().remove(0);
        engine
            .fail("b", &final_attempt.dispatch_id, "terminal", true)
            .await
            .unwrap();
        engine
            .fail("b", &final_attempt.dispatch_id, "stale transient", false)
            .await
            .unwrap();
        assert!(engine.claim_dispatches("b", true).await.unwrap().is_empty());
        let terminal: bool = sqlx::query_scalar("SELECT terminal_failure FROM dispatch_commands")
            .fetch_one(&engine.store.pool)
            .await
            .unwrap();
        assert!(terminal);
    }

    #[tokio::test]
    async fn forwards_attachment_metadata_and_orders_pending_messages() {
        let (engine, _temp) = setup(
            r#"
                [[replication_groups]]
                id = "one"
                endpoints = ["a-room", "b-room"]
            "#,
        )
        .await;
        let mut first = create("first", "a-room", "first-origin", None);
        first.attachments.push(AttachmentDescriptor {
            kind: 1,
            filename: "photo.jpg".into(),
            mime_type: "image/jpeg".into(),
            size_bytes: 42,
        });
        engine.ingest("a", first).await.unwrap();
        engine
            .ingest("a", create("second", "a-room", "second-origin", None))
            .await
            .unwrap();

        let first = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text, "first");
        assert_eq!(first[0].attachments[0].filename, "photo.jpg");
        engine
            .acknowledge("b", &first[0].dispatch_id, "first-copy")
            .await
            .unwrap();
        let second = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "second");
    }
}
