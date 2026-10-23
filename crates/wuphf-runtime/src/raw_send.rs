//! The runtime handle for owner-authored raw sends: turns a send request into
//! core dispatches, injects them into the live delivery channel, and registers
//! the send with the [`Observer`] so its acknowledgement logs an outgoing
//! observation. Cloned into services (the observer gRPC surface) that accept
//! sends while the runtime runs.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use wuphf_core::{Core, CoreError};
use wuphf_model::{
    ObservedDirection, ObservedMessage, Platform, RAW_SENDER, RawOutbound, Dispatch,
};
use wuphf_storage::Storage;

use crate::Observer;

/// Accepts owner-authored sends and feeds them into the running delivery loop.
pub struct RawMessageSender<S> {
    core: Arc<Core<S>>,
    observer: Arc<Observer<S>>,
    dispatch_tx: mpsc::Sender<Dispatch>,
}

// Manual impl: `S` itself need not be `Clone` behind the `Arc`s.
impl<S> Clone for RawMessageSender<S> {
    fn clone(&self) -> Self {
        Self {
            core: Arc::clone(&self.core),
            observer: Arc::clone(&self.observer),
            dispatch_tx: self.dispatch_tx.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum RawSendError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("dispatch channel closed; the runtime is shutting down")]
    RuntimeStopped,
}

impl<S> RawMessageSender<S>
where
    S: Storage,
{
    pub(crate) fn new(
        core: Arc<Core<S>>,
        observer: Arc<Observer<S>>,
        dispatch_tx: mpsc::Sender<Dispatch>,
    ) -> Self {
        Self {
            core,
            observer,
            dispatch_tx,
        }
    }

    /// Send an owner-authored text to a platform chat. Enqueues the direct
    /// dispatch and any mirror fan-out for delivery, and returns a receiver
    /// that resolves with the observed (outgoing) row once the connector
    /// acknowledges the send with its native message id. Dropping the receiver
    /// is fine; the send proceeds and is logged regardless.
    pub async fn send(
        &self,
        outbound: RawOutbound,
    ) -> Result<oneshot::Receiver<ObservedMessage>, RawSendError> {
        let observed = self.observed_template(&outbound);
        let outcome = self.core.send_raw(outbound).await?;
        // Track before injecting the dispatch: the acknowledgement must not be
        // able to race past an untracked send.
        let settled = self.observer.track_outbound(outcome.direct.id, observed);
        self.inject(outcome.direct).await?;
        for mirror in outcome.mirrored {
            self.inject(mirror).await?;
        }
        Ok(settled)
    }

    /// The observation-log row for a raw send, complete except for the native
    /// message id its acknowledgement will supply.
    fn observed_template(&self, outbound: &RawOutbound) -> ObservedMessage {
        let platform = self
            .core
            .connector_platform(&outbound.connector_id)
            .unwrap_or_else(|| Platform::Other(outbound.connector_id.0.clone()));
        ObservedMessage {
            connector_id: outbound.connector_id.clone(),
            platform,
            chat_id: outbound.chat_id.clone(),
            thread_id: outbound.thread_id.clone(),
            platform_message_id: String::new(),
            sender: RAW_SENDER.to_owned(),
            sender_name: None,
            participant: None,
            text: Some(outbound.text.clone()),
            attachments: Vec::new(),
            reply_to_platform_message_id: outbound.reply_to_platform_message_id.clone(),
            direction: ObservedDirection::Outgoing,
            timestamp_ms: outbound.timestamp_ms,
            deleted: false,
            edited: false,
        }
    }

    async fn inject(&self, dispatch: Dispatch) -> Result<(), RawSendError> {
        self.dispatch_tx
            .send(dispatch)
            .await
            .map_err(|_| RawSendError::RuntimeStopped)
    }
}

/// Wall-clock epoch milliseconds for stamping a send. Saturates at zero if the
/// clock reads before the epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}
