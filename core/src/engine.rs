//! Application facade for authenticated ingestion and durable dispatch processing.

use crate::{
    config::Config, delivery::DeliveryPolicy, pipeline::InboundPipeline, repository::Repository,
    routing::RoutingTable, store::Store,
};
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use wuphf_proto::v1::{CreateMessage, DispatchCreateMessage};

/// Result returned after processing an inbound normalized event.
pub use crate::pipeline::IngestResult;

#[derive(Debug, Error)]
/// Application-level failure surfaced while processing connector operations.
pub enum EngineError {
    /// A connector submitted a message from an endpoint it does not own.
    #[error("connector is not authorized for endpoint '{0}'")]
    UnauthorizedEndpoint(String),
    /// A reply referenced an origin-side message copy unknown to the core.
    #[error("reply target '{0}' does not identify a known message copy at the origin endpoint")]
    UnknownReply(String),
    /// A connector reported an outcome for a dispatch outside its ownership.
    #[error("dispatch does not belong to the authenticated connector")]
    UnauthorizedDispatch,
    /// A repeated acknowledgement supplied a different target platform ID.
    #[error("dispatch acknowledgement conflicts with its recorded platform message ID")]
    ConflictingAcknowledgement,
    /// A durable SQLite operation failed.
    #[error("persistent state error: {0}")]
    Database(#[from] sqlx::Error),
    /// Persisted normalized metadata could not be decoded.
    #[error("stored attachment metadata cannot be decoded: {0}")]
    Metadata(#[from] serde_json::Error),
}

#[derive(Clone)]
/// Core application facade coordinating pipeline, persistence, and delivery policy.
pub struct Engine {
    /// Validated runtime configuration used for authentication and public inspection.
    pub config: Arc<Config>,
    /// Durable state transactions for dispatch processing.
    repository: Repository,
    /// Timing rules for leases and transient retries.
    delivery: DeliveryPolicy,
    /// Ordered inbound create-message processing path.
    inbound: InboundPipeline,
}

impl Engine {
    /// Constructs a core engine from validated configuration and initialized storage.
    pub fn new(config: Arc<Config>, store: Store) -> Self {
        let delivery = DeliveryPolicy::from_config(&config.delivery);
        let routing = RoutingTable::from_config(&config);
        let repository = Repository::new(store);
        Self {
            config,
            repository: repository.clone(),
            delivery,
            inbound: InboundPipeline::new(repository, routing),
        }
    }

    /// Returns the acknowledgement lease applied after sending a dispatch command.
    pub fn acknowledgement_timeout(&self) -> Duration {
        self.delivery.acknowledgement_timeout()
    }

    /// Checks connector registration credentials against validated configuration.
    pub fn authenticate_connector(&self, connector_id: &str, shared_token: &str) -> bool {
        self.config.connector_token(connector_id) == Some(shared_token)
    }

    /// Processes an authenticated normalized create-message submission.
    pub async fn ingest(
        &self,
        connector_id: &str,
        message: CreateMessage,
    ) -> Result<IngestResult, EngineError> {
        self.inbound.process_create(connector_id, message).await
    }

    /// Claims commands currently deliverable to a connector instance.
    pub async fn claim_dispatches(
        &self,
        connector_id: &str,
        allow_early_retry: bool,
    ) -> Result<Vec<DispatchCreateMessage>, EngineError> {
        self.repository
            .claim_dispatches(connector_id, &self.delivery, allow_early_retry)
            .await
    }

    /// Records successful creation of a target platform message copy.
    pub async fn acknowledge(
        &self,
        connector_id: &str,
        dispatch_id: &str,
        platform_message_id: &str,
    ) -> Result<(), EngineError> {
        self.repository
            .acknowledge(connector_id, dispatch_id, platform_message_id)
            .await
    }

    /// Records connector delivery failure and returns transient retry delay, if applicable.
    pub async fn fail(
        &self,
        connector_id: &str,
        dispatch_id: &str,
        reason: &str,
        permanent: bool,
    ) -> Result<Option<Duration>, EngineError> {
        self.repository
            .fail(connector_id, dispatch_id, reason, permanent, &self.delivery)
            .await
    }
}

#[cfg(test)]
/// Unit tests for engine-facing behavioral invariants.
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wuphf_proto::v1::AttachmentDescriptor;

    /// Builds a persisted two-endpoint engine fixture.
    async fn setup() -> (Engine, TempDir) {
        let temp = tempfile::tempdir().unwrap();
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
            temp.path().join("engine.sqlite").display()
        ))
        .unwrap();
        config.validate().unwrap();
        let config = Arc::new(config);
        let store = Store::open(&config).await.unwrap();
        (Engine::new(config, store), temp)
    }

    /// Builds a normalized source message for the engine fixture.
    fn create(event: &str, platform_id: &str, reply: Option<&str>) -> CreateMessage {
        CreateMessage {
            event_id: event.into(),
            endpoint_id: "a-room".into(),
            platform_message_id: platform_id.into(),
            sender_id: "sender".into(),
            sender_display_name: "Sender".into(),
            sent_at_unix_ms: 10,
            text: event.into(),
            reply_to_platform_message_id: reply.map(str::to_owned),
            attachments: Vec::new(),
        }
    }

    #[tokio::test]
    /// Deduplicates repeated source events and prevents acknowledged copies from re-entering.
    async fn deduplicates_ingestion_and_observed_dispatched_copies() {
        let (engine, _temp) = setup().await;
        let created = engine
            .ingest("a", create("one", "a-one", None))
            .await
            .unwrap();
        let duplicate = engine
            .ingest("a", create("one", "a-one", None))
            .await
            .unwrap();
        assert_eq!(duplicate.logical_message_id, created.logical_message_id);
        assert!(!duplicate.was_new);
        let dispatch = engine.claim_dispatches("b", false).await.unwrap().remove(0);
        engine
            .acknowledge("b", &dispatch.dispatch_id, "b-one")
            .await
            .unwrap();

        let mut observed = create("observed", "b-one", None);
        observed.endpoint_id = "b-room".into();
        assert!(!engine.ingest("b", observed).await.unwrap().was_new);
        assert!(engine
            .claim_dispatches("a", false)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    /// Resolves replies only after target parent copies exist and preserves metadata.
    async fn reply_waits_for_target_copy_and_preserves_attachment_metadata() {
        let (engine, _temp) = setup().await;
        let mut parent = create("parent", "a-parent", None);
        parent.attachments.push(AttachmentDescriptor {
            kind: 1,
            filename: "photo.jpg".into(),
            mime_type: "image/jpeg".into(),
            size_bytes: 42,
        });
        engine.ingest("a", parent).await.unwrap();
        engine
            .ingest("a", create("reply", "a-reply", Some("a-parent")))
            .await
            .unwrap();
        let first = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(first[0].attachments[0].filename, "photo.jpg");
        engine
            .acknowledge("b", &first[0].dispatch_id, "b-parent")
            .await
            .unwrap();
        let reply = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(
            reply[0].reply_to_platform_message_id.as_deref(),
            Some("b-parent")
        );
    }

    #[tokio::test]
    /// Leaves terminal failures blocked and keeps repeated failure processing idempotent.
    async fn failure_policy_is_idempotent_and_terminal_failure_stops_delivery() {
        let (engine, _temp) = setup().await;
        engine
            .ingest("a", create("one", "a-one", None))
            .await
            .unwrap();
        let first = engine.claim_dispatches("b", false).await.unwrap().remove(0);
        assert_eq!(
            engine
                .fail("b", &first.dispatch_id, "try", false)
                .await
                .unwrap(),
            Some(Duration::from_millis(1_000))
        );
        assert!(engine
            .claim_dispatches("b", false)
            .await
            .unwrap()
            .is_empty());
        let retried = engine.claim_dispatches("b", true).await.unwrap().remove(0);
        engine
            .fail("b", &retried.dispatch_id, "terminal", true)
            .await
            .unwrap();
        engine
            .fail("b", &retried.dispatch_id, "stale", false)
            .await
            .unwrap();
        assert!(engine.claim_dispatches("b", true).await.unwrap().is_empty());
    }

    #[tokio::test]
    /// Leases commands once and delivers subsequent endpoint work in order.
    async fn claims_once_until_acknowledged_and_preserves_endpoint_order() {
        let (engine, _temp) = setup().await;
        engine
            .ingest("a", create("one", "a-one", None))
            .await
            .unwrap();
        engine
            .ingest("a", create("two", "a-two", None))
            .await
            .unwrap();

        let first = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text, "one");
        assert!(engine
            .claim_dispatches("b", false)
            .await
            .unwrap()
            .is_empty());
        engine
            .acknowledge("b", &first[0].dispatch_id, "b-one")
            .await
            .unwrap();

        let second = engine.claim_dispatches("b", false).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "two");
    }
}
