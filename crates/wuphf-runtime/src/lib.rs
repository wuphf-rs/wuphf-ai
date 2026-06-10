use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use wuphf_core::{Core, CoreError, IngestOutcome};
use wuphf_model::{
    ConnectorId, DispatchAcknowledgement, DispatchId, EventPayload, NormalizedEvent,
    PlatformAddress, StoredDispatch,
};
use wuphf_storage::Storage;

pub mod connector;
pub mod grpc;
pub mod media;

/// A platform identity migration request: `(connector, old address, new address)`.
pub type Migration = (ConnectorId, PlatformAddress, PlatformAddress);

/// A terminal dispatch failure reported by a connector: `(dispatch, reason)`.
pub type DispatchFailure = (DispatchId, String);

pub struct Runtime<S> {
    core: Arc<Core<S>>,
    event_rx: mpsc::Receiver<NormalizedEvent>,
    acknowledgement_rx: mpsc::Receiver<DispatchAcknowledgement>,
    migration_rx: mpsc::Receiver<Migration>,
    failure_rx: mpsc::Receiver<DispatchFailure>,
    dispatch_tx: mpsc::Sender<StoredDispatch>,
}

pub struct RuntimeChannels {
    pub events: mpsc::Sender<NormalizedEvent>,
    pub acknowledgements: mpsc::Sender<DispatchAcknowledgement>,
    pub migrations: mpsc::Sender<Migration>,
    pub failures: mpsc::Sender<DispatchFailure>,
    pub dispatches: mpsc::Receiver<StoredDispatch>,
}

impl<S> Runtime<S>
where
    S: Storage,
{
    pub fn channelled(core: Arc<Core<S>>, capacity: usize) -> (Self, RuntimeChannels) {
        let (event_tx, event_rx) = mpsc::channel(capacity);
        let (acknowledgement_tx, acknowledgement_rx) = mpsc::channel(capacity);
        let (migration_tx, migration_rx) = mpsc::channel(capacity);
        let (failure_tx, failure_rx) = mpsc::channel(capacity);
        let (dispatch_tx, dispatch_rx) = mpsc::channel(capacity);
        (
            Self {
                core,
                event_rx,
                acknowledgement_rx,
                migration_rx,
                failure_rx,
                dispatch_tx,
            },
            RuntimeChannels {
                events: event_tx,
                acknowledgements: acknowledgement_tx,
                migrations: migration_tx,
                failures: failure_tx,
                dispatches: dispatch_rx,
            },
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
        loop {
            tokio::select! {
                event = self.event_rx.recv() => match event {
                    Some(event) => {
                        let outcome = self.ingest(event).await?;
                        for dispatch in outcome.dispatches().iter().cloned() {
                            self.send(dispatch).await?;
                        }
                    }
                    None => break,
                },
                acknowledgement = self.acknowledgement_rx.recv() => {
                    if let Some(acknowledgement) = acknowledgement {
                        debug!(dispatch_id = %acknowledgement.dispatch_id, "dispatch acknowledged");
                        for dispatch in self.core.acknowledge_dispatch(acknowledgement).await? {
                            self.send(dispatch).await?;
                        }
                    }
                }
                migration = self.migration_rx.recv() => {
                    if let Some((connector_id, old, new)) = migration {
                        info!(
                            connector = %connector_id.0,
                            old = %old.chat_id,
                            new = %new.chat_id,
                            "migrating chat identity",
                        );
                        self.core.migrate_identity(connector_id, old, new).await?;
                    }
                }
                failure = self.failure_rx.recv() => {
                    if let Some((dispatch_id, reason)) = failure {
                        warn!(dispatch_id = %dispatch_id, reason, "dispatch failed permanently, dead-lettering");
                        for dispatch in self.core.fail_dispatch(dispatch_id, reason).await? {
                            self.send(dispatch).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Ingest one connector event and log the core's verdict. This is the single
    /// observation point for inbound traffic: the core itself stays log-free and
    /// communicates through [`IngestOutcome`].
    async fn ingest(&self, event: NormalizedEvent) -> Result<IngestOutcome, RuntimeError> {
        let connector = event.source.connector_id.0.clone();
        let chat = event.source.address.chat_id.clone();
        let kind = match &event.payload {
            EventPayload::MessageCreate(_) => "create",
            EventPayload::MessageEdit { .. } => "edit",
            EventPayload::MessageDelete { .. } => "delete",
            EventPayload::ReactionAdd(_) => "reaction",
        };
        let outcome = self.core.ingest(event).await?;
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

    async fn send(&self, dispatch: StoredDispatch) -> Result<(), RuntimeError> {
        self.dispatch_tx
            .send(dispatch)
            .await
            .map_err(|_| RuntimeError::DispatchChannelClosed)
    }
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
