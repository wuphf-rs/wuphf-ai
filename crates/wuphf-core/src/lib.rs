use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use wuphf_model::{
    ConnectorId, DispatchId, EndpointId, EventPayload, MediaId, Message, MessageId,
    NormalizedEvent, Platform, ReplicaSetId, Rule, SideKind, StoredDispatch,
};
use wuphf_router::{Router, RouterError, RoutingOutcome};
use wuphf_storage::{Storage, StorageError};

pub use wuphf_router::{ConnectorInfo, RoutingConfig};

mod acknowledge;
mod ingest;
mod resolve;
mod send;

pub use send::{RAW_SENDER, RawOutbound, RawSendOutcome};

pub struct Core<S> {
    storage: Arc<S>,
    router: Router<S>,
    config: Arc<RoutingConfig>,
    /// Conversations whose mirror thread is mid-provisioning, keyed by replica
    /// set. Guards against a burst of messages each provisioning a duplicate
    /// thread before the first provision is acknowledged. The lock is never
    /// held across an await.
    provisioning: Mutex<HashMap<ReplicaSetId, ProvisionState>>,
}

/// A conversation's in-flight thread provisioning: the dispatches whose
/// acknowledgement establishes the thread, and the messages held until it does.
#[derive(Default)]
struct ProvisionState {
    pending: HashSet<DispatchId>,
    stash: Vec<StashedMessage>,
}

/// A message received while its conversation's mirror thread was still being
/// provisioned, replayed once the thread exists.
struct StashedMessage {
    message: Message,
    origin_endpoint: EndpointId,
    provision_title: String,
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

/// The identity-owning side of a rule: the source side if one exists, otherwise
/// the first single side. Used to key the conversation deterministically.
fn identity_side_index(rule: &Rule) -> usize {
    rule.sides
        .iter()
        .position(|side| matches!(side.kind, SideKind::Source { .. }))
        .or_else(|| {
            rule.sides
                .iter()
                .position(|side| matches!(side.kind, SideKind::Single { .. }))
        })
        .expect("a rule has at least one single or source side")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    Routed {
        message_id: MessageId,
        routing: RoutingOutcome,
        dispatches: Vec<StoredDispatch>,
    },
    /// Held because the conversation's mirror thread is still being
    /// provisioned; the message is replayed once the thread exists.
    Deferred {
        message_id: MessageId,
    },
    Ignored(IgnoreReason),
}

impl IngestOutcome {
    pub fn dispatches(&self) -> &[StoredDispatch] {
        match self {
            Self::Routed { dispatches, .. } => dispatches,
            Self::Deferred { .. } | Self::Ignored(_) => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgnoreReason {
    DuplicateEvent,
    SentMessageEcho,
    Unmatched,
    /// An edit that leaves the stored text unchanged: a redundant report or our
    /// own mirrored edit echoed back by the platform.
    RedundantEdit,
    /// A delete of an already-revoked message: a redundant report or our own
    /// mirrored delete echoed back by the platform.
    RedundantDelete,
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
