use crate::{
    engine::EngineError,
    repository::{Ingestion, Repository},
    routing::RoutingTable,
};
use std::collections::BTreeSet;
use wuphf_proto::v1::CreateMessage;

#[derive(Clone)]
pub(crate) struct InboundPipeline {
    repository: Repository,
    routing: RoutingTable,
}

#[derive(Debug)]
pub struct IngestResult {
    pub logical_message_id: String,
    pub target_connectors: BTreeSet<String>,
    pub was_new: bool,
}

impl InboundPipeline {
    pub(crate) fn new(repository: Repository, routing: RoutingTable) -> Self {
        Self {
            repository,
            routing,
        }
    }

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
