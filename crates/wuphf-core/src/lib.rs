use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use wuphf_model::{
    ConnectorId, DispatchId, EndpointId, EventPayload, IgnoreReason, IngestOutcome, MediaId,
    MessageId, NormalizedEvent, Platform, ReplicaSetId,
};
use wuphf_router::{Router, RouterError};
use wuphf_storage::{Storage, StorageError};

pub use wuphf_router::RoutingConfig;

mod acknowledge;
mod ingest;
mod resolve;
mod send;

pub struct Core<S> {
    storage: Arc<S>,
    router: Router<S>,
    config: Arc<RoutingConfig>,
    /// Conversations whose mirror thread is mid-provisioning, keyed by replica
    /// set, holding the dispatch ids whose acknowledgement establishes the
    /// thread. Guards against a burst of messages each provisioning a duplicate
    /// thread before the first provision settles. This is a rebuildable cache of
    /// storage (see [`Core::recover_provisioning`]); the messages held until the
    /// thread exists live in the durable `provisioning_stash` table, not here, so
    /// a crash cannot lose them. The lock is never held across an await.
    provisioning: Mutex<HashMap<ReplicaSetId, ProvisionState>>,
}

/// A conversation's in-flight thread provisioning: the dispatches whose
/// acknowledgement (or failure) establishes the thread or gives up. The stashed
/// messages themselves are persisted in storage, not here.
#[derive(Default)]
struct ProvisionState {
    pending: HashSet<DispatchId>,
}

impl<S> Core<S>
where
    S: Storage,
{
    pub fn new(storage: Arc<S>, config: Arc<RoutingConfig>) -> Self {
        Self {
            router: Router::new(Arc::clone(&storage), Arc::clone(&config)),
            config,
            storage,
            provisioning: Mutex::new(HashMap::new()),
        }
    }

    /// Shared handle to the backing storage, for read-only consumers (e.g. the
    /// observation tap and observer service) that query alongside the core.
    pub fn storage(&self) -> Arc<S> {
        Arc::clone(&self.storage)
    }

    /// The platform a connector bridges, from static registration config. `None`
    /// when the connector is not configured (an event from an unknown connector);
    /// the caller falls back to a connector-derived label.
    pub fn connector_platform(&self, connector_id: &ConnectorId) -> Option<Platform> {
        self.config
            .connector(connector_id)
            .map(|info| info.platform.clone())
    }

    pub async fn ingest(&self, event: NormalizedEvent) -> Result<IngestOutcome, CoreError> {
        let source = event.source;
        let Some(origin_endpoint) = self.resolve_origin(&source).await? else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let origin = self.storage.endpoint(origin_endpoint).await?;
        let replica_set_id = self
            .storage
            .replica_set_for_endpoint(origin_endpoint)
            .await?
            .ok_or(CoreError::OriginWithoutReplicaSet(origin_endpoint))?;

        match event.payload {
            EventPayload::MessageCreate(inbound) => {
                self.ingest_create(source, origin_endpoint, origin, replica_set_id, inbound)
                    .await
            }
            EventPayload::MessageEdit {
                platform_message_id,
                text,
            } => {
                self.ingest_edit(origin_endpoint, platform_message_id, text)
                    .await
            }
            EventPayload::MessageDelete {
                platform_message_id,
            } => {
                self.ingest_delete(origin_endpoint, platform_message_id)
                    .await
            }
            EventPayload::ReactionSet {
                platform_message_id,
                reactor,
                reactor_name,
                emoji,
                // from_me is unused in the source-to-text direction: the bridge
                // never applies a native reaction the source could echo back, so
                // every reported reaction (including the owner's own) mirrors.
                from_me: _,
            } => {
                self.ingest_reaction(
                    origin_endpoint,
                    platform_message_id,
                    reactor,
                    reactor_name,
                    emoji,
                )
                .await
            }
            EventPayload::ReceiptSet {
                platform_message_id,
                observer: _,
                from_me,
                kind,
            } => {
                self.ingest_receipt(origin_endpoint, platform_message_id, from_me, kind)
                    .await
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Router(#[from] RouterError),
    #[error("origin endpoint {0} resolved to no replica set")]
    OriginWithoutReplicaSet(EndpointId),
    #[error("rule references connector {0} which is not registered")]
    UnknownConnector(ConnectorId),
    #[error("reaction ingestion is not yet implemented")]
    UnsupportedEvent,
    #[error("deleted message {0} maps from a copy but is not stored")]
    DeleteTargetNotStored(MessageId),
    #[error("edited message {0} maps from a copy but is not stored")]
    EditTargetNotStored(MessageId),
    #[error("reacted message {0} maps from a copy but is not stored")]
    ReactionTargetNotStored(MessageId),
    #[error("receipted message {0} maps from a copy but is not stored")]
    ReceiptTargetNotStored(MessageId),
    #[error("attachment media {0} is not uploaded to the core")]
    UnknownMedia(MediaId),
    #[error("attachment metadata for media {0} differs from the core record")]
    MediaMetadataMismatch(MediaId),
}

#[cfg(test)]
mod tests;
