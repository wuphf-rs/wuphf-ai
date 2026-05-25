//! Transactional persistence for logical messages, message copies, and dispatch state.

use crate::{
    config::EndpointConfig,
    delivery::{now_unix_ms, DeliveryPolicy},
    engine::EngineError,
    store::Store,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;
use wuphf_proto::v1::{AttachmentDescriptor, CreateMessage, DispatchCreateMessage};

#[derive(Clone)]
/// Durable SQLite repository used by the application and inbound pipeline layers.
pub(crate) struct Repository {
    /// Initialized SQLite store containing migrations and configured topology.
    store: Store,
}

/// Persistence result when an inbound event is submitted.
pub(crate) enum Ingestion {
    /// The event or observed copy was already associated with this logical message.
    Existing(String),
    /// A new logical message and its durable dispatch work were created.
    Created(String),
}

#[derive(Serialize, Deserialize)]
/// JSON representation of attachment metadata persisted with a logical message.
struct StoredAttachment {
    /// Protocol attachment kind numeric value.
    kind: i32,
    /// Connector-provided file name, when present.
    filename: String,
    /// Connector-provided MIME type, when present.
    mime_type: String,
    /// Connector-provided byte size metadata.
    size_bytes: u64,
}

impl Repository {
    /// Wraps an initialized SQLite store as the transactional persistence boundary.
    pub(crate) fn new(store: Store) -> Self {
        Self { store }
    }

    /// Atomically deduplicates an inbound create, records its origin copy, and persists fan-out.
    pub(crate) async fn ingest(
        &self,
        connector_id: &str,
        message: &CreateMessage,
        targets: &[EndpointConfig],
    ) -> Result<Ingestion, EngineError> {
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
            return Ok(Ingestion::Existing(existing));
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
            return Ok(Ingestion::Existing(existing));
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
        .bind(serialize_attachments(&message.attachments)?)
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
        Ok(Ingestion::Created(logical_id))
    }

    /// Atomically leases all currently deliverable commands for one connector.
    ///
    /// When `allow_early_retry` is true, unfinished commands may be reclaimed
    /// immediately after connector reconnect rather than waiting for a prior
    /// acknowledgement lease or failure deadline to expire.
    pub(crate) async fn claim_dispatches(
        &self,
        connector_id: &str,
        policy: &DeliveryPolicy,
        allow_early_retry: bool,
    ) -> Result<Vec<DispatchCreateMessage>, EngineError> {
        let now = now_unix_ms();
        let lease_until = policy.deadline_after(policy.acknowledgement_timeout());
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
            dispatches.push(dispatch_from_row(row, dispatch_id)?);
        }
        tx.commit().await?;
        Ok(dispatches)
    }

    /// Records successful dispatch completion and the resulting target message copy.
    ///
    /// Repeated acknowledgement with the same target message ID is idempotent.
    pub(crate) async fn acknowledge(
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

        sqlx::query(
            "INSERT INTO message_copies
             (id, logical_message_id, endpoint_id, platform_message_id, source, dispatch_id)
             VALUES (?, ?, ?, ?, 'dispatch', ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(row.get::<String, _>("logical_message_id"))
        .bind(row.get::<String, _>("target_endpoint_id"))
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

    /// Records a transient or terminal dispatch failure and returns retry delay, if any.
    pub(crate) async fn fail(
        &self,
        connector_id: &str,
        dispatch_id: &str,
        reason: &str,
        permanent: bool,
        policy: &DeliveryPolicy,
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
        if row.get::<bool, _>("terminal_failure") {
            tx.commit().await?;
            return Ok(None);
        }
        if row.get::<String, _>("status") == "failed" && !permanent {
            let retry_at: Option<i64> = row.get("next_attempt_at_unix_ms");
            tx.commit().await?;
            return Ok(retry_at.map(remaining_until));
        }
        let retry_delay = if permanent {
            None
        } else {
            Some(policy.retry_delay(row.get("attempt_count")))
        };
        let retry_at = retry_delay.map(|delay| policy.deadline_after(delay));
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
}

/// Serializes wire-level attachment descriptors for persistence without media bytes.
fn serialize_attachments(
    attachments: &[AttachmentDescriptor],
) -> Result<String, serde_json::Error> {
    serde_json::to_string(
        &attachments
            .iter()
            .map(|item| StoredAttachment {
                kind: item.kind,
                filename: item.filename.clone(),
                mime_type: item.mime_type.clone(),
                size_bytes: item.size_bytes,
            })
            .collect::<Vec<_>>(),
    )
}

/// Converts a joined dispatch query row back into its wire-level delivery command.
fn dispatch_from_row(
    row: sqlx::sqlite::SqliteRow,
    dispatch_id: String,
) -> Result<DispatchCreateMessage, serde_json::Error> {
    let metadata: Vec<StoredAttachment> =
        serde_json::from_str(row.get::<&str, _>("attachments_json"))?;
    Ok(DispatchCreateMessage {
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
    })
}

/// Calculates the remaining non-negative delay until a persisted retry deadline.
fn remaining_until(deadline: i64) -> Duration {
    Duration::from_millis(
        deadline
            .saturating_sub(now_unix_ms())
            .try_into()
            .unwrap_or_default(),
    )
}
