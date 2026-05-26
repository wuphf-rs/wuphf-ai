//! Operator-facing inspection and recovery of durable dispatch commands.

use crate::{repository::Repository, store::Store, EngineError};

/// One durable dispatch row exposed for operator diagnosis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchRecord {
    /// Durable dispatch command identifier.
    pub id: String,
    /// Logical message being delivered.
    pub logical_message_id: String,
    /// Endpoint receiving the dispatched copy.
    pub target_endpoint_id: String,
    /// Connector instance responsible for target endpoint.
    pub target_connector_id: String,
    /// Current durable delivery state.
    pub status: String,
    /// Number of delivery claims made by core.
    pub attempt_count: i64,
    /// Endpoint-local order enforced for this command.
    pub endpoint_sequence: i64,
    /// Epoch timestamp of the most recent delivery attempt, if sent.
    pub last_attempted_at_unix_ms: Option<i64>,
    /// Epoch timestamp after which automatic retry is eligible, if scheduled.
    pub next_attempt_at_unix_ms: Option<i64>,
    /// Most recent connector-reported failure, if any.
    pub last_error: Option<String>,
    /// Whether automatic delivery is halted pending operator recovery.
    pub terminal_failure: bool,
}

/// Optional filter for querying durable dispatch records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchFilter {
    /// Restricts query to dispatches for one connector instance.
    pub connector_id: Option<String>,
    /// Restricts query to dispatches for one endpoint.
    pub endpoint_id: Option<String>,
    /// When true, returns terminal failed dispatches only.
    pub terminal_only: bool,
    /// Maximum number of records returned, clamped by the repository.
    pub limit: u32,
}

impl Default for DispatchFilter {
    /// Builds a broad query limited to a bounded diagnostic page.
    fn default() -> Self {
        Self {
            connector_id: None,
            endpoint_id: None,
            terminal_only: false,
            limit: 100,
        }
    }
}

/// Core-owned operator service for durable dispatch state.
#[derive(Clone)]
pub struct AdminService {
    repository: Repository,
}

impl AdminService {
    /// Creates operator service over initialized durable state.
    pub fn new(store: Store) -> Self {
        Self {
            repository: Repository::new(store),
        }
    }

    /// Lists dispatch state matching supplied filter.
    pub async fn list_dispatches(
        &self,
        filter: &DispatchFilter,
    ) -> Result<Vec<DispatchRecord>, EngineError> {
        let records = self.repository.list_dispatches(filter).await?;
        tracing::debug!(
            terminal_only = filter.terminal_only,
            connector_id = filter.connector_id.as_deref(),
            endpoint_id = filter.endpoint_id.as_deref(),
            limit = filter.limit,
            result_count = records.len(),
            "durable dispatches inspected"
        );
        Ok(records)
    }

    /// Clears terminal failure for one dispatch so normal delivery may resume.
    ///
    /// Attempt history and failure text remain persisted for diagnosis.
    pub async fn retry_dispatch(&self, dispatch_id: &str) -> Result<(), EngineError> {
        self.repository.retry_terminal_dispatch(dispatch_id).await?;
        tracing::info!(%dispatch_id, "terminal dispatch reopened for delivery");
        Ok(())
    }
}

#[cfg(test)]
/// Unit tests for operator dispatch recovery use cases.
mod tests {
    use super::*;
    use crate::{Config, Engine};
    use std::sync::Arc;
    use tempfile::TempDir;
    use wuphf_proto::v1::CreateMessage;

    /// Builds engine and admin service sharing one temporary database.
    async fn setup() -> (Engine, AdminService, TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(&format!(
            r#"
                [server]
                listen = "127.0.0.1:1"
                [database]
                url = "sqlite://{}"
                [[connectors]]
                id = "a"
                token = "a"
                [[connectors]]
                id = "b"
                token = "b"
                [[endpoints]]
                id = "a-room"
                connector_id = "a"
                address = "a"
                [[endpoints]]
                id = "b-room"
                connector_id = "b"
                address = "b"
                [[replication_groups]]
                id = "rooms"
                endpoints = ["a-room", "b-room"]
            "#,
            temp.path().join("admin.sqlite").display()
        ))
        .unwrap();
        config.validate().unwrap();
        let config = Arc::new(config);
        let store = Store::open(&config).await.unwrap();
        (
            Engine::new(config, store.clone()),
            AdminService::new(store),
            temp,
        )
    }

    /// Builds one normalized source event for terminal-failure setup.
    fn create() -> CreateMessage {
        CreateMessage {
            event_id: "event".into(),
            endpoint_id: "a-room".into(),
            platform_message_id: "a-message".into(),
            sender_id: "sender".into(),
            sender_display_name: "Sender".into(),
            sent_at_unix_ms: 1,
            text: "hello".into(),
            reply_to_platform_message_id: None,
            attachments: Vec::new(),
        }
    }

    #[tokio::test]
    /// Lists terminal failure and reopens it for normal dispatch claiming.
    async fn retries_a_terminal_dispatch_without_losing_attempt_history() {
        let (engine, admin, _temp) = setup().await;
        engine.ingest("a", create()).await.unwrap();
        let dispatch = engine.claim_dispatches("b", false).await.unwrap().remove(0);
        engine
            .fail("b", &dispatch.dispatch_id, "terminal", true)
            .await
            .unwrap();

        let records = admin
            .list_dispatches(&DispatchFilter {
                terminal_only: true,
                ..DispatchFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].terminal_failure);
        assert_eq!(records[0].attempt_count, 1);
        assert_eq!(records[0].endpoint_sequence, 1);
        assert!(records[0].last_attempted_at_unix_ms.is_some());

        admin.retry_dispatch(&dispatch.dispatch_id).await.unwrap();
        let error = admin
            .retry_dispatch(&dispatch.dispatch_id)
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::DispatchNotTerminal(_)));
        let reclaimed = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(reclaimed[0].dispatch_id, dispatch.dispatch_id);
        let reopened = admin
            .list_dispatches(&DispatchFilter::default())
            .await
            .unwrap();
        assert_eq!(reopened[0].attempt_count, 2);
        assert!(!reopened[0].terminal_failure);
    }
}
