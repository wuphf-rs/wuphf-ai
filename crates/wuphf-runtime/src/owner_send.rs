//! The runtime handle for owner messages: turns a send request into
//! core dispatches, injects them into the live delivery channel, and registers
//! the send with the [`Observer`] so its acknowledgement logs an outgoing
//! observation. Cloned into services (the observer gRPC surface) that accept
//! sends while the runtime runs.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use std::collections::BTreeSet;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use wuphf_core::{Core, CoreError, PostOutcome, Released};
use wuphf_model::{
    Action, Dispatch, HealthPause, OWNER_AUTHOR, ObservedDirection, ObservedMessage, OwnerMessage,
    Platform,
};
use wuphf_storage::Storage;

use crate::Observer;
use crate::live_sessions::LiveSessions;

/// Accepts owner-authored sends and feeds them into the running delivery loop.
pub struct OwnerMessageSender<S> {
    core: Arc<Core<S>>,
    observer: Arc<Observer<S>>,
    dispatch_tx: mpsc::Sender<Dispatch>,
}

// Manual impl: `S` itself need not be `Clone` behind the `Arc`s.
impl<S> Clone for OwnerMessageSender<S> {
    fn clone(&self) -> Self {
        Self {
            core: Arc::clone(&self.core),
            observer: Arc::clone(&self.observer),
            dispatch_tx: self.dispatch_tx.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum OwnerSendError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("dispatch channel closed; the runtime is shutting down")]
    RuntimeStopped,
}

impl<S> OwnerMessageSender<S>
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

    /// Send an owner-authored message to a platform chat. Enqueues the direct
    /// dispatch and any mirror fan-out for delivery, and returns a receiver
    /// that resolves with the observed (outgoing) row once the connector
    /// acknowledges the send with its native message id. Dropping the receiver
    /// is fine; the send proceeds and is logged regardless.
    pub async fn send(
        &self,
        outbound: OwnerMessage,
    ) -> Result<oneshot::Receiver<ObservedMessage>, OwnerSendError> {
        let mut observed = self.observed_template(&outbound);
        let outcome = self.core.send_owner_message(outbound).await?;
        // The core resolved the media ids to their stored attachments.
        if let Action::Send { message, .. } = &outcome.direct.action {
            observed.attachments = message.attachments.clone();
        }
        // Track before injecting the dispatch: the acknowledgement must not be
        // able to race past an untracked send.
        let settled = self.observer.track_outbound(outcome.direct.id, observed);
        self.inject(outcome.direct).await?;
        for mirror in outcome.mirrored {
            self.inject(mirror).await?;
        }
        Ok(settled)
    }

    /// Tell the owner something about the bridge itself (a new release)
    /// through every notice target, returning how many messages that took.
    pub async fn announce(&self, text: &str) -> Result<usize, OwnerSendError> {
        let dispatches = self.core.announce(text, now_ms()).await?;
        let sent = dispatches.len();
        for dispatch in dispatches {
            self.inject(dispatch).await?;
        }
        Ok(sent)
    }

    /// The core, for reads the panel's pages make (plan 084's pending posts).
    pub fn core(&self) -> &Arc<Core<S>> {
        &self.core
    }

    /// The owner's ✅ on a pending post (plan 084): post it now.
    pub async fn confirm_post(
        &self,
        id: wuphf_model::MessageId,
    ) -> Result<PostOutcome, OwnerSendError> {
        let outcome = self.core.confirm_post(id, now_ms()).await?;
        self.inject_posted(&outcome).await?;
        Ok(outcome)
    }

    /// Post `text` typed in the panel opened in a topic (plan 084).
    pub async fn compose_post(
        &self,
        connector: &wuphf_model::ConnectorId,
        address: &wuphf_model::ChatAddress,
        text: &str,
    ) -> Result<PostOutcome, OwnerSendError> {
        let outcome = self
            .core
            .compose_post(connector, address, text, now_ms())
            .await?;
        self.inject_posted(&outcome).await?;
        Ok(outcome)
    }

    /// Delete the mirror copies of the stories that expired (plan 084), when
    /// `stories.expire_copies` is on, returning how many deletes that took.
    pub async fn expire_stories(&self) -> Result<usize, OwnerSendError> {
        let deletes = self.core.expire_stories(now_ms()).await?;
        let count = deletes.len();
        for delete in deletes {
            self.inject(delete).await?;
        }
        Ok(count)
    }

    /// Sweep expired stories every `every`, until the task is aborted.
    pub fn spawn_story_expiry(&self, every: std::time::Duration) -> tokio::task::JoinHandle<()> {
        let sender = self.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(every);
            loop {
                ticks.tick().await;
                match sender.expire_stories().await {
                    Ok(0) => {}
                    Ok(count) => {
                        tracing::info!(count, "deleted the mirror copies of expired stories")
                    }
                    Err(OwnerSendError::RuntimeStopped) => return,
                    Err(error) => tracing::warn!(%error, "could not expire stories"),
                }
            }
        })
    }

    /// Ask the connectors for the reaction counts due a poll (plan 085),
    /// returning how many chats were asked. A chat whose connector is away
    /// stays due, and is asked once it is back.
    pub async fn poll_reactions(&self, sessions: &LiveSessions) -> Result<usize, OwnerSendError> {
        let now = now_ms();
        let mut asked = 0;
        for poll in self.core.due_reaction_polls(now).await? {
            match sessions.refresh_reactions(&poll.connector_id.0, &poll.chat_id, poll.since_ms) {
                Ok(()) => {
                    self.core.mark_reaction_polled(&poll, now);
                    asked += 1;
                }
                Err(reason) => {
                    tracing::debug!(%reason, chat = %poll.chat_id, "reaction counts not polled")
                }
            }
        }
        Ok(asked)
    }

    /// Look for reaction-count polls due every `every`, until the task is
    /// aborted; each chat is polled every `channels.counts_poll_minutes`.
    pub fn spawn_reaction_polls(
        &self,
        sessions: Arc<LiveSessions>,
        every: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        let sender = self.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(every);
            loop {
                ticks.tick().await;
                match sender.poll_reactions(&sessions).await {
                    Ok(_) => {}
                    Err(OwnerSendError::RuntimeStopped) => return,
                    Err(error) => tracing::warn!(%error, "could not poll reaction counts"),
                }
            }
        })
    }

    async fn inject_posted(&self, outcome: &PostOutcome) -> Result<(), OwnerSendError> {
        if let PostOutcome::Posted(dispatches) = outcome {
            for dispatch in dispatches {
                self.inject(dispatch.clone()).await?;
            }
        }
        Ok(())
    }

    /// Hold dispatches into `pauses` from now on (plan 079).
    pub fn set_health_pauses(&self, pauses: BTreeSet<HealthPause>) {
        self.core.set_health_pauses(pauses);
    }

    /// Send what `pause` held, in order, dropping what was held before
    /// `cutoff_ms` (see [`Core::release_held`]).
    pub async fn release_held(
        &self,
        pause: &HealthPause,
        cutoff_ms: i64,
    ) -> Result<Released, OwnerSendError> {
        let released = self.core.release_held(pause, cutoff_ms).await?;
        for dispatch in &released.dispatches {
            self.inject(dispatch.clone()).await?;
        }
        Ok(released)
    }

    /// The observation-log row for an owner message, complete except for the native
    /// message id its acknowledgement will supply and the attachments the core
    /// resolves.
    fn observed_template(&self, outbound: &OwnerMessage) -> ObservedMessage {
        let platform = self
            .core
            .connector_platform(&outbound.connector_id)
            .unwrap_or_else(|| Platform::Other(outbound.connector_id.0.clone()));
        ObservedMessage {
            connector_id: outbound.connector_id.clone(),
            platform,
            chat_id: outbound.chat_id.clone(),
            thread_id: outbound.thread_id.clone(),
            native_id: String::new(),
            author_id: OWNER_AUTHOR.to_owned(),
            author_name: None,
            participant: None,
            text: (!outbound.text.is_empty()).then(|| outbound.text.clone()),
            attachments: Vec::new(),
            reply_to_native_id: outbound.reply_to_native_id.clone(),
            direction: ObservedDirection::Outgoing,
            timestamp_ms: outbound.timestamp_ms,
            deleted: false,
            edited: false,
            sent_by: outbound.client.clone(),
        }
    }

    async fn inject(&self, dispatch: Dispatch) -> Result<(), OwnerSendError> {
        self.dispatch_tx
            .send(dispatch)
            .await
            .map_err(|_| OwnerSendError::RuntimeStopped)
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
