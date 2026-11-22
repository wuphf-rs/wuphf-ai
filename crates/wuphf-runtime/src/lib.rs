use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use wuphf_core::{Core, CoreError};
use wuphf_model::{
    ConnectorId, Dispatch, DispatchAck, DispatchFailed, Event, EventKind, InboundWork,
    IngestOutcome, Platform, ReceiptKind,
};
use wuphf_storage::Storage;

pub mod auth;
pub mod connector;
pub mod connector_settings;
pub mod grpc;
pub mod media;
pub mod observation;
pub mod observer_service;
pub mod owner_send;
pub mod settings_auth;

pub use observation::Observer;
pub use owner_send::{OwnerMessageSender, OwnerSendError};

pub struct Runtime<S> {
    core: Arc<Core<S>>,
    observer: Arc<Observer<S>>,
    inbound_rx: mpsc::Receiver<InboundWork>,
    acknowledgement_rx: mpsc::Receiver<DispatchAck>,
    failure_rx: mpsc::Receiver<DispatchFailed>,
    dispatch_tx: mpsc::Sender<Dispatch>,
    /// Per-connector ingest worker queue depth. Sized to the runtime capacity so
    /// a connector replaying an offline backlog buffers ahead without stalling
    /// the demux that feeds the other connectors' workers.
    worker_capacity: usize,
}

pub struct RuntimeChannels {
    pub inbound: mpsc::Sender<InboundWork>,
    pub acknowledgements: mpsc::Sender<DispatchAck>,
    pub failures: mpsc::Sender<DispatchFailed>,
    pub dispatches: mpsc::Receiver<Dispatch>,
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

    /// A handle that accepts owner messages and feeds them into this
    /// runtime's delivery loop. Take clones before calling [`Runtime::run`],
    /// which consumes the runtime.
    pub fn owner_sender(&self) -> OwnerMessageSender<S> {
        OwnerMessageSender::new(
            Arc::clone(&self.core),
            Arc::clone(&self.observer),
            self.dispatch_tx.clone(),
        )
    }

    pub async fn run(mut self) -> Result<(), RuntimeError> {
        // Snapshot the pending set before recovery replays any orphaned stash:
        // recover_provisioning rebuilds the in-memory provisioning cache and
        // replays stashes whose provision settled while we were down, enqueuing
        // fresh dispatches. Capturing pending first keeps those replays out of
        // the redelivery set, so each dispatch is sent exactly once.
        let pending = self.core.pending_dispatches().await?;
        let recovered = self.core.recover_provisioning().await?;
        if !pending.is_empty() {
            info!(
                count = pending.len(),
                "redelivering dispatches pending from a previous run"
            );
        }
        if !recovered.is_empty() {
            info!(
                count = recovered.len(),
                "replaying provisioning stashes orphaned by a previous run"
            );
        }
        for dispatch in pending {
            self.send(dispatch).await?;
        }
        for dispatch in recovered {
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
                        let (dispatch_id, native_id) = (
                            acknowledgement.dispatch_id,
                            acknowledgement.native_id.clone(),
                        );
                        for dispatch in self.core.acknowledge_dispatch(acknowledgement).await? {
                            self.send(dispatch).await?;
                        }
                        // An owner message settles its outgoing observation here; every
                        // other dispatch is a no-op. A failure only loses the
                        // observation row, never the delivery, so warn and go on.
                        if let Err(error) = self
                            .observer
                            .settle_outbound(dispatch_id, &native_id)
                            .await
                        {
                            warn!(dispatch_id = %dispatch_id, %error, "failed to record outgoing observation");
                        }
                    }
                }
                failure = self.failure_rx.recv() => {
                    if let Some(DispatchFailed { dispatch_id, reason }) = failure {
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

    async fn send(&self, dispatch: Dispatch) -> Result<(), RuntimeError> {
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
    dispatch_tx: mpsc::Sender<Dispatch>,
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
/// event is ingested through the core; a migration rebinds an address and a
/// chat update records chat profiles, and neither yields dispatches. All run on
/// the connector's worker so they apply in the order the connector sent them.
async fn process_work<S>(
    core: &Core<S>,
    observer: &Observer<S>,
    work: InboundWork,
) -> Result<Vec<Dispatch>, RuntimeError>
where
    S: Storage,
{
    match work {
        InboundWork::Event(event) => Ok(ingest_event(core, observer, event)
            .await?
            .dispatches()
            .to_vec()),
        InboundWork::Notice {
            connector_id,
            kind,
            detail,
            media_ids,
        } => {
            warn!(
                connector = %connector_id.0,
                ?kind,
                detail,
                "connector reported a health problem",
            );
            // The core is clock-free, so the send time comes from here.
            let dispatches = core
                .notify_owners(connector_id, kind, &detail, &media_ids, now_ms())
                .await?;
            Ok(dispatches)
        }
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
        InboundWork::ChatUpdate {
            connector_id,
            updates,
            snapshot,
        } => {
            let reported = updates.len();
            let dispatches = core.update_chats(&connector_id, updates, snapshot).await?;
            debug!(
                connector = %connector_id.0,
                updates = reported,
                snapshot,
                dispatches = dispatches.len(),
                "chat profiles updated",
            );
            Ok(dispatches)
        }
    }
}

/// Ingest one connector event and log the core's verdict. This is the single
/// observation point for inbound traffic: the core itself stays log-free and
/// communicates through [`IngestOutcome`].
async fn ingest_event<S>(
    core: &Core<S>,
    observer: &Observer<S>,
    event: Event,
) -> Result<IngestOutcome, RuntimeError>
where
    S: Storage,
{
    let connector = event.source.connector_id.0.clone();
    let chat = event.source.address.chat_id.clone();
    let kind = match &event.payload {
        EventKind::NewMessage(_) => "create",
        EventKind::Edit { .. } => "edit",
        EventKind::Delete { .. } => "delete",
        EventKind::Reaction { .. } => "reaction",
        EventKind::Receipt {
            kind: ReceiptKind::Delivered,
            ..
        } => "delivery_receipt",
        EventKind::Receipt {
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
            blocked,
        } => debug!(
            connector,
            chat,
            kind,
            message_id = %message_id,
            dispatches = dispatches.len(),
            blocked,
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

/// Wall-clock milliseconds for work the core stamps but does not time itself.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or_default()
}
