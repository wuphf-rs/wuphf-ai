use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use wuphf_core::{Core, CoreError, IngestOutcome};
use wuphf_model::{
    ConnectorId, DispatchAcknowledgement, DispatchId, EventPayload, NormalizedEvent, Platform,
    PlatformAddress, ReceiptKind, StoredDispatch,
};
use wuphf_storage::Storage;

pub mod connector;
pub mod grpc;
pub mod media;
pub mod observation;
pub mod observer_service;
pub mod raw_send;

pub use observation::Observer;
pub use raw_send::{RawMessageSender, RawSendError};

/// A terminal dispatch failure reported by a connector: `(dispatch, reason)`.
pub type DispatchFailure = (DispatchId, String);

/// Ordered inbound work from one connector: a normalized event or an identity
/// migration. Both travel one per-connector path so the connector's frame order
/// is preserved end to end. A migration that precedes a message is applied
/// before that message is ingested, so the message cannot mint a fresh endpoint
/// under an address the migration was meant to rebind.
pub enum InboundWork {
    Event(NormalizedEvent),
    Migration {
        connector_id: ConnectorId,
        old: PlatformAddress,
        new: PlatformAddress,
    },
}

impl InboundWork {
    fn connector_id(&self) -> &ConnectorId {
        match self {
            InboundWork::Event(event) => &event.source.connector_id,
            InboundWork::Migration { connector_id, .. } => connector_id,
        }
    }
}

pub struct Runtime<S> {
    core: Arc<Core<S>>,
    observer: Arc<Observer<S>>,
    inbound_rx: mpsc::Receiver<InboundWork>,
    acknowledgement_rx: mpsc::Receiver<DispatchAcknowledgement>,
    failure_rx: mpsc::Receiver<DispatchFailure>,
    dispatch_tx: mpsc::Sender<StoredDispatch>,
    /// Per-connector ingest worker queue depth. Sized to the runtime capacity so
    /// a connector replaying an offline backlog buffers ahead without stalling
    /// the demux that feeds the other connectors' workers.
    worker_capacity: usize,
}

pub struct RuntimeChannels {
    pub inbound: mpsc::Sender<InboundWork>,
    pub acknowledgements: mpsc::Sender<DispatchAcknowledgement>,
    pub failures: mpsc::Sender<DispatchFailure>,
    pub dispatches: mpsc::Receiver<StoredDispatch>,
}

impl<S> Runtime<S>
where
    S: Storage,
{
    /// Build the runtime and its wiring. The returned [`Observer`] handle backs
    /// read-only consumers (the observer service, a TUI): it shares the same
    /// storage and receives every captured message live. It is returned here so a
    /// caller can wire consumers before [`Runtime::run`] consumes the runtime.
    pub fn channelled(
        core: Arc<Core<S>>,
        capacity: usize,
    ) -> (Self, RuntimeChannels, Arc<Observer<S>>) {
        let (inbound_tx, inbound_rx) = mpsc::channel(capacity);
        let (acknowledgement_tx, acknowledgement_rx) = mpsc::channel(capacity);
        let (failure_tx, failure_rx) = mpsc::channel(capacity);
        let (dispatch_tx, dispatch_rx) = mpsc::channel(capacity);
        let observer = Arc::new(Observer::new(core.storage()));
        (
            Self {
                core,
                observer: Arc::clone(&observer),
                inbound_rx,
                acknowledgement_rx,
                failure_rx,
                dispatch_tx,
                worker_capacity: capacity,
            },
            RuntimeChannels {
                inbound: inbound_tx,
                acknowledgements: acknowledgement_tx,
                failures: failure_tx,
                dispatches: dispatch_rx,
            },
            observer,
        )
    }

    /// A handle that accepts owner-authored raw sends and feeds them into this
    /// runtime's delivery loop. Take clones before calling [`Runtime::run`],
    /// which consumes the runtime.
    pub fn raw_sender(&self) -> RawMessageSender<S> {
        RawMessageSender::new(
            Arc::clone(&self.core),
            Arc::clone(&self.observer),
            self.dispatch_tx.clone(),
        )
    }

    pub async fn run(mut self) -> Result<(), RuntimeError> {
        let pending = self.core.pending_dispatches().await?;
        if !pending.is_empty() {
            info!(
                count = pending.len(),
                "redelivering dispatches pending from a previous run"
            );
        }
        for dispatch in pending {
            self.send(dispatch).await?;
        }
        // Each connector gets its own ingest worker so one connector's offline
        // backlog cannot starve the others: the run loop only demuxes inbound
        // work to the right worker, while the workers process concurrently and
        // feed their dispatches back into the shared dispatch channel. A
        // connector's events and identity migrations share one worker queue, so
        // they apply in the connector's frame order (a migration before the
        // message it precedes). Per-connector ordering holds because a worker
        // drains its queue serially and a conversation lives on one connector. A
        // worker's first fatal core error is surfaced here so it still aborts the
        // runtime as before.
        let mut workers: HashMap<ConnectorId, mpsc::Sender<InboundWork>> = HashMap::new();
        let (worker_error_tx, mut worker_error_rx) = mpsc::channel::<RuntimeError>(1);
        loop {
            tokio::select! {
                work = self.inbound_rx.recv() => match work {
                    Some(work) => {
                        let connector_id = work.connector_id().clone();
                        let worker = workers.entry(connector_id).or_insert_with(|| {
                            spawn_ingest_worker(
                                self.core.clone(),
                                self.observer.clone(),
                                self.dispatch_tx.clone(),
                                worker_error_tx.clone(),
                                self.worker_capacity,
                            )
                        });
                        // A closed worker queue means its task aborted on a fatal
                        // error; the worker_error_rx branch carries the cause and
                        // ends the run, so dropping the work here is fine.
                        let _ = worker.send(work).await;
                    }
                    None => break,
                },
                Some(error) = worker_error_rx.recv() => return Err(error),
                acknowledgement = self.acknowledgement_rx.recv() => {
                    if let Some(acknowledgement) = acknowledgement {
                        debug!(dispatch_id = %acknowledgement.dispatch_id, "dispatch acknowledged");
                        let (dispatch_id, platform_message_id) = (
                            acknowledgement.dispatch_id,
                            acknowledgement.platform_message_id.clone(),
                        );
                        for dispatch in self.core.acknowledge_dispatch(acknowledgement).await? {
                            self.send(dispatch).await?;
                        }
                        // A raw send settles its outgoing observation here; every
                        // other dispatch is a no-op. A failure only loses the
                        // observation row, never the delivery, so warn and go on.
                        if let Err(error) = self
                            .observer
                            .settle_outbound(dispatch_id, &platform_message_id)
                            .await
                        {
                            warn!(dispatch_id = %dispatch_id, %error, "failed to record outgoing observation");
                        }
                    }
                }
                failure = self.failure_rx.recv() => {
                    if let Some((dispatch_id, reason)) = failure {
                        warn!(dispatch_id = %dispatch_id, reason, "dispatch failed permanently, dead-lettering");
                        self.observer.discard_outbound(dispatch_id);
                        for dispatch in self.core.fail_dispatch(dispatch_id, reason).await? {
                            self.send(dispatch).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn send(&self, dispatch: StoredDispatch) -> Result<(), RuntimeError> {
        self.dispatch_tx
            .send(dispatch)
            .await
            .map_err(|_| RuntimeError::DispatchChannelClosed)
    }
}

/// Spawn a per-connector ingest worker. It drains its queue serially, processing
/// each item through the core and forwarding any resulting dispatches to the
/// shared dispatch channel. Events and identity migrations share the queue so
/// they apply in arrival order. On the first fatal core error it reports the
/// cause and stops, which closes its queue and ends the runtime.
fn spawn_ingest_worker<S>(
    core: Arc<Core<S>>,
    observer: Arc<Observer<S>>,
    dispatch_tx: mpsc::Sender<StoredDispatch>,
    error_tx: mpsc::Sender<RuntimeError>,
    capacity: usize,
) -> mpsc::Sender<InboundWork>
where
    S: Storage,
{
    let (work_tx, mut work_rx) = mpsc::channel::<InboundWork>(capacity);
    tokio::spawn(async move {
        while let Some(work) = work_rx.recv().await {
            let dispatches = match process_work(core.as_ref(), observer.as_ref(), work).await {
                Ok(dispatches) => dispatches,
                Err(error) => {
                    let _ = error_tx.send(error).await;
                    return;
                }
            };
            for dispatch in dispatches {
                // A closed dispatch channel means the runtime is shutting down;
                // stop quietly rather than reporting it as a fatal error.
                if dispatch_tx.send(dispatch).await.is_err() {
                    return;
                }
            }
        }
    });
    work_tx
}

/// Apply one unit of inbound work and return any dispatches it produced. An
/// event is ingested through the core; a migration rebinds an address and yields
/// no dispatches. Both run on the connector's worker so they apply in the order
/// the connector sent them.
async fn process_work<S>(
    core: &Core<S>,
    observer: &Observer<S>,
    work: InboundWork,
) -> Result<Vec<StoredDispatch>, RuntimeError>
where
    S: Storage,
{
    match work {
        InboundWork::Event(event) => Ok(ingest_event(core, observer, event)
            .await?
            .dispatches()
            .to_vec()),
        InboundWork::Migration {
            connector_id,
            old,
            new,
        } => {
            info!(
                connector = %connector_id.0,
                old = %old.chat_id,
                new = %new.chat_id,
                "migrating chat identity",
            );
            core.migrate_identity(connector_id, old, new).await?;
            Ok(Vec::new())
        }
    }
}

/// Ingest one connector event and log the core's verdict. This is the single
/// observation point for inbound traffic: the core itself stays log-free and
/// communicates through [`IngestOutcome`].
async fn ingest_event<S>(
    core: &Core<S>,
    observer: &Observer<S>,
    event: NormalizedEvent,
) -> Result<IngestOutcome, RuntimeError>
where
    S: Storage,
{
    let connector = event.source.connector_id.0.clone();
    let chat = event.source.address.chat_id.clone();
    let kind = match &event.payload {
        EventPayload::MessageCreate(_) => "create",
        EventPayload::MessageEdit { .. } => "edit",
        EventPayload::MessageDelete { .. } => "delete",
        EventPayload::ReactionSet { .. } => "reaction",
        EventPayload::ReceiptSet {
            kind: ReceiptKind::Delivered,
            ..
        } => "delivery_receipt",
        EventPayload::ReceiptSet {
            kind: ReceiptKind::Read,
            ..
        } => "read_receipt",
    };
    // Capture into the observation log before routing decides anything, so a chat
    // that matches no rule is still recorded. Observation is a side concern: a
    // failure is logged but never fails ingest.
    let platform = core
        .connector_platform(&event.source.connector_id)
        .unwrap_or_else(|| Platform::Other(event.source.connector_id.0.clone()));
    if let Err(error) = observer.observe(&event, platform).await {
        warn!(connector, chat, kind, %error, "failed to record observation");
    }
    let outcome = core.ingest(event).await?;
    match &outcome {
        IngestOutcome::Routed {
            message_id,
            dispatches,
            ..
        } => debug!(
            connector,
            chat,
            kind,
            message_id = %message_id,
            dispatches = dispatches.len(),
            "event routed",
        ),
        IngestOutcome::Deferred { message_id } => debug!(
            connector,
            chat,
            kind,
            message_id = %message_id,
            "event deferred until mirror thread provisioning completes",
        ),
        IngestOutcome::Ignored(reason) => {
            debug!(connector, chat, kind, ?reason, "event ignored")
        }
    }
    Ok(outcome)
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("dispatch receiver closed while commands were pending")]
    DispatchChannelClosed,
}

#[cfg(test)]
mod tests;
