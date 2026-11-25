use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use thiserror::Error;
use wuphf_model::{
    ConnectorId, ConversationId, DispatchId, EndpointId, Event, EventKind, HealthPause,
    IgnoreReason, IngestOutcome, MediaId, MessageId, NoticeKind, Platform,
};
use wuphf_storage::{Storage, StorageError};

mod acknowledge;
mod chat_state;
mod config;
mod hold;
mod ingest;
mod notes;
mod notify;
mod posts;
mod profile;
mod refusal;
mod reply_reaction;
mod resolve;
mod router;
mod send;

pub use config::{
    ChatStateSettings, MatchedRule, RoutingConfig, RoutingConfigError, RoutingSource,
};
pub use hold::Released;
pub use posts::{PENDING_POST_TTL_MS, PendingPost, PostOutcome};
use router::Router;
pub use router::RouterError;

pub struct Core<S> {
    storage: Arc<S>,
    /// The routing config, read once per operation (see [`RoutingSource`]).
    routing: Arc<dyn RoutingSource>,
    /// Conversations whose mirror thread is mid-provisioning, holding the dispatch ids whose acknowledgement establishes the
    /// thread. Guards against a burst of messages each provisioning a duplicate
    /// thread before the first provision settles. This is a rebuildable cache of
    /// storage (see [`Core::recover_provisioning`]); the messages held until the
    /// thread exists live in the durable `provisioning_stash` table, not here, so
    /// a crash cannot lose them. The lock is never held across an await.
    provisioning: Mutex<HashMap<ConversationId, ProvisionState>>,
    /// The last health state each connector reported, so a connector repeating
    /// itself does not page the owner over and over. See
    /// [`Core::notify_owners`]. Deliberately in memory: a restart re-notifies
    /// about a connector that is still down.
    notices: Mutex<HashMap<ConnectorId, NoticeKind>>,
    /// The directions of rules held by the health checks (plan 079), set by
    /// the runtime. See [`Core::set_health_pauses`].
    health_pauses: RwLock<Arc<BTreeSet<HealthPause>>>,
    /// Posts waiting for the owner's ✅ on their preview (plan 084), by the
    /// command message's id. In memory: a restart forgets them, and the owner
    /// sends the command again.
    posts: Mutex<HashMap<MessageId, PendingPost>>,
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
    pub fn new(storage: Arc<S>, routing: impl RoutingSource) -> Self {
        Self {
            routing: Arc::new(routing),
            storage,
            provisioning: Mutex::new(HashMap::new()),
            notices: Mutex::new(HashMap::new()),
            health_pauses: RwLock::default(),
            posts: Mutex::new(HashMap::new()),
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
        self.routing
            .routing()
            .connector(connector_id)
            .map(|info| info.platform.clone())
    }

    /// A router over one operation's routing snapshot.
    fn with_routing(&self, routing: &Arc<RoutingConfig>) -> Router<S> {
        Router::new(Arc::clone(&self.storage), Arc::clone(routing))
    }

    pub async fn ingest(&self, event: Event) -> Result<IngestOutcome, CoreError> {
        let routing = self.routing.routing();
        let source = event.source;
        // Before routing, so a chat no rule matches still learns its timer and
        // the message's own dispatches carry the new one.
        let timer_changes = match &event.payload {
            EventKind::NewMessage(inbound) => Some(
                self.observe_message_timer(&routing, &source, inbound.disappearing)
                    .await?,
            ),
            _ => None,
        };
        let Some(origin_endpoint) = self.resolve_origin(&routing, &source).await? else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let origin = self.storage.endpoint(origin_endpoint).await?;
        let conversation_id = self
            .storage
            .conversation_for_endpoint(origin_endpoint)
            .await?
            .ok_or(CoreError::OriginWithoutConversation(origin_endpoint))?
            .id;

        match event.payload {
            EventKind::NewMessage(inbound) => {
                let mut outcome = self
                    .ingest_create(
                        &routing,
                        source.clone(),
                        origin_endpoint,
                        origin,
                        conversation_id,
                        inbound,
                    )
                    .await?;
                // A timer change the message revealed is noted ahead of the
                // message. Only a routed message posts it: otherwise the
                // mirror has no thread yet, and its info card will show the
                // timer.
                if let (Some(changes), IngestOutcome::Routed { dispatches, .. }) =
                    (&timer_changes, &mut outcome)
                {
                    let notes = self.message_timer_notes(&routing, &source, changes).await?;
                    dispatches.splice(0..0, notes);
                }
                Ok(outcome)
            }
            EventKind::Edit { native_id, text } => {
                self.ingest_edit(&routing, origin_endpoint, native_id, text)
                    .await
            }
            EventKind::Delete { native_id } => {
                self.ingest_delete(&routing, origin_endpoint, native_id)
                    .await
            }
            EventKind::Reaction {
                native_id,
                reactor,
                reactor_name,
                emoji,
                from_me,
            } => {
                let change =
                    ingest::ReactionChange::reported(reactor, reactor_name, emoji, from_me);
                self.ingest_reaction(&routing, &origin, native_id, change)
                    .await
            }
            EventKind::ReactionSet {
                native_id,
                reactions,
            } => {
                self.ingest_reaction_set(&routing, &origin, native_id, reactions)
                    .await
            }
            EventKind::Receipt {
                native_id,
                observer: _,
                from_me,
                kind,
            } => {
                self.ingest_receipt(&routing, origin_endpoint, native_id, from_me, kind)
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
    #[error("origin endpoint {0} belongs to no conversation")]
    OriginWithoutConversation(EndpointId),
    #[error("rule references connector {0} which is not registered")]
    UnknownConnector(ConnectorId),
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
