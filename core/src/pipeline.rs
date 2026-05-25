//! Typed processing pipeline for normalized inbound connector operations.

use crate::{
    engine::EngineError,
    repository::{Ingestion, Repository},
    routing::RoutingTable,
};
use std::collections::BTreeSet;
use wuphf_proto::v1::CreateMessage;

#[derive(Clone)]
/// Core-owned stages applied to an inbound normalized create-message operation.
pub(crate) struct InboundPipeline {
    /// Transactional durable-state boundary used by ingestion.
    repository: Repository,
    /// Precomputed routing and ownership policy used before persistence.
    routing: RoutingTable,
}

#[derive(Debug)]
/// Outcome of processing one inbound message submission.
pub struct IngestResult {
    /// Logical message identity corresponding to the observed event.
    pub logical_message_id: String,
    /// Connectors whose online sessions should be checked for new dispatches.
    pub target_connectors: BTreeSet<String>,
    /// Whether this event created new logical-message and dispatch state.
    pub was_new: bool,
}

impl InboundPipeline {
    /// Builds an inbound pipeline from durable state and compiled routing policy.
    pub(crate) fn new(repository: Repository, routing: RoutingTable) -> Self {
        Self {
            repository,
            routing,
        }
    }

    /// Authorizes, ingests, deduplicates, and fans out a normalized create message.
    pub(crate) async fn process_create(
        &self,
        connector_id: &str,
        message: CreateMessage,
    ) -> Result<IngestResult, EngineError> {
        self.authorize_origin(connector_id, &message)?;
        let targets = self.routing.target_endpoints(&message.endpoint_id);
        let target_connectors = targets
            .iter()
            .map(|endpoint| endpoint.connector_id.clone())
            .collect();

        match self
            .repository
            .ingest(connector_id, &message, targets)
            .await?
        {
            Ingestion::Existing(logical_message_id) => Ok(IngestResult {
                logical_message_id,
                target_connectors: BTreeSet::new(),
                was_new: false,
            }),
            Ingestion::Created(logical_message_id) => Ok(IngestResult {
                logical_message_id,
                target_connectors,
                was_new: true,
            }),
        }
    }

    /// Rejects connector submissions for endpoints they do not own.
    fn authorize_origin(
        &self,
        connector_id: &str,
        message: &CreateMessage,
    ) -> Result<(), EngineError> {
        if self
            .routing
            .connector_owns_endpoint(connector_id, &message.endpoint_id)
        {
            Ok(())
        } else {
            Err(EngineError::UnauthorizedEndpoint(
                message.endpoint_id.clone(),
            ))
        }
    }
}
