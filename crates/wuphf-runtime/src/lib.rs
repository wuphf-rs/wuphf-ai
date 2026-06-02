use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use wuphf_core::{Core, CoreError};
use wuphf_model::{
    ConnectorId, DispatchAcknowledgement, DispatchId, NormalizedEvent, PlatformAddress,
    StoredDispatch,
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
        for dispatch in self.core.pending_dispatches().await? {
            self.send(dispatch).await?;
        }
        loop {
            tokio::select! {
                event = self.event_rx.recv() => match event {
                    Some(event) => {
                        let outcome = self.core.ingest(event).await?;
                        for dispatch in outcome.dispatches().iter().cloned() {
                            self.send(dispatch).await?;
                        }
                    }
                    None => break,
                },
                acknowledgement = self.acknowledgement_rx.recv() => {
                    if let Some(acknowledgement) = acknowledgement {
                        for dispatch in self.core.acknowledge_dispatch(acknowledgement).await? {
                            self.send(dispatch).await?;
                        }
                    }
                }
                migration = self.migration_rx.recv() => {
                    if let Some((connector_id, old, new)) = migration {
                        self.core.migrate_identity(connector_id, old, new).await?;
                    }
                }
                failure = self.failure_rx.recv() => {
                    if let Some((dispatch_id, reason)) = failure {
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

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("dispatch receiver closed while commands were pending")]
    DispatchChannelClosed,
}

#[cfg(test)]
mod tests;
