//! The shared connector session client. Every Rust connector maintains its
//! session against the core through this module: the reconnect-with-backoff
//! loop, bounded send retry, permanent-error dead-lettering, and frame
//! dispatch are identical across platforms and live here once. Per-connector
//! facts (platform, capabilities, the optional platform-specific
//! permanent-error classifier) are supplied through [`Options`].

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Endpoint, Uri};
use tracing::{debug, error, info, warn};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_client::ConnectorTransportClient;

use crate::sender::{Delivery, Failure, Sender};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const MAX_SEND_ATTEMPTS: u32 = 3;
const SEND_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Buffers connector-produced frames (events, acknowledgements, failures)
/// ahead of the gRPC stream so a slow network does not stall normalization.
const OUTBOUND_CAPACITY: usize = 64;

/// Connector-specific classifier for delivery failures not explicitly marked
/// permanent by the sender.
pub type PermanentFailureClassifier = dyn Fn(&Failure) -> bool + Send + Sync;

/// The per-connector facts the shared session client needs. Each connector
/// maps its own config into this struct; the shared client imports no
/// connector's config.
pub struct Options {
    /// The core's gRPC address (e.g. `127.0.0.1:50051`); a scheme is added
    /// when missing.
    pub core_address: String,
    pub connector_id: String,
    pub platform: pb::Platform,
    pub supports_threads: bool,
    pub supports_topic_create: bool,
    /// Whether the connector stamps chats' disappearing-messages timers on what
    /// it sends and reports the timers it observes. Part of the registration,
    /// so it must match the connector's entry in the core config.
    pub supports_disappearing_timers: bool,
    /// The connector's core token, sent as `authorization: Bearer <token>` on
    /// the session.
    pub token: String,
    /// When set, classifies platform-specific failures as permanent in
    /// addition to explicit [`Failure::Permanent`] marking.
    pub is_permanent: Option<Arc<PermanentFailureClassifier>>,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("invalid core address: {0}")]
    Address(#[from] tonic::codegen::http::uri::InvalidUri),
    #[error("connect to core: {0}")]
    Connect(#[from] tonic::transport::Error),
    #[error("the core token is not a valid header value")]
    Token(#[from] tonic::metadata::errors::InvalidMetadataValue),
    #[error(transparent)]
    Transport(#[from] tonic::Status),
    #[error("core rejected connector operation: {0}")]
    Rejected(String),
    #[error("core closed the connector session stream")]
    StreamClosed,
    #[error("core returned an empty connector frame")]
    EmptyFrame,
    #[error("send dispatch {dispatch_id}: {reason}")]
    Delivery { dispatch_id: String, reason: String },
}

pub struct Client {
    opts: Options,
}

impl Client {
    pub fn new(opts: Options) -> Self {
        Self { opts }
    }

    fn is_permanent(&self, failure: &Failure) -> bool {
        failure.is_permanent()
            || self
                .opts
                .is_permanent
                .as_ref()
                .is_some_and(|classify| classify(failure))
    }

    /// Maintain a connector session against the core, reconnecting with
    /// exponential backoff when a session ends for any reason other than
    /// cancellation. Unacknowledged dispatches are redelivered by the core on
    /// reconnect, so a dropped session does not lose work. Returns when
    /// `cancel` fires.
    pub async fn run<S: Sender>(
        &self,
        sender: S,
        mut inputs: mpsc::Receiver<pb::ClientFrame>,
        cancel: CancellationToken,
    ) -> Result<(), SessionError> {
        let sender = Arc::new(sender);
        let mut backoff = INITIAL_BACKOFF;
        loop {
            let result = self
                .run_session(&sender, &mut inputs, &cancel, || backoff = INITIAL_BACKOFF)
                .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    warn!(%error, ?backoff, "connector session ended, reconnecting");
                    tokio::select! {
                        _ = cancel.cancelled() => return Ok(()),
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    async fn run_session<S: Sender>(
        &self,
        sender: &Arc<S>,
        inputs: &mut mpsc::Receiver<pb::ClientFrame>,
        cancel: &CancellationToken,
        on_connected: impl FnOnce(),
    ) -> Result<(), SessionError> {
        let endpoint = Endpoint::new(core_uri(&self.opts.core_address)?)?;
        let channel = endpoint.connect().await?;
        let mut transport = ConnectorTransportClient::new(channel);

        // Registration, connector events and dispatch settlements all share one
        // outbound channel so the core observes them in connector order.
        let (outbound_tx, outbound_rx) = mpsc::channel::<pb::ClientFrame>(OUTBOUND_CAPACITY);
        outbound_tx
            .try_send(pb::ClientFrame {
                body: Some(pb::client_frame::Body::Registration(pb::Registration {
                    connector_id: self.opts.connector_id.clone(),
                    platform: self.opts.platform.into(),
                    capabilities: Some(pb::Capabilities {
                        supports_threads: self.opts.supports_threads,
                        supports_topic_create: self.opts.supports_topic_create,
                        supports_disappearing_timers: self.opts.supports_disappearing_timers,
                    }),
                    // This SDK declares no runtime settings yet.
                    settings: None,
                })),
            })
            .map_err(|_| SessionError::StreamClosed)?;
        let mut request = tonic::Request::new(ReceiverStream::new(outbound_rx));
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.opts.token).parse()?,
        );
        let mut frames = transport.open_session(request).await?.into_inner();
        on_connected();
        info!(connector_id = %self.opts.connector_id, "connector session registered with core");

        let mut inputs_open = true;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                frame = frames.message() => match frame {
                    Ok(Some(frame)) => self.handle_frame(frame, sender, &outbound_tx).await?,
                    Ok(None) => return Err(SessionError::StreamClosed),
                    Err(status) => return Err(SessionError::Transport(status)),
                },
                input = inputs.recv(), if inputs_open => match input {
                    Some(frame) => {
                        outbound_tx.send(frame).await.map_err(|_| SessionError::StreamClosed)?;
                    }
                    // A closed event source must not end the session: pending
                    // dispatches still need to be served and settled.
                    None => inputs_open = false,
                },
            }
        }
    }

    async fn handle_frame<S: Sender>(
        &self,
        frame: pb::ServerFrame,
        sender: &Arc<S>,
        outbound: &mpsc::Sender<pb::ClientFrame>,
    ) -> Result<(), SessionError> {
        match frame.body {
            Some(pb::server_frame::Body::MessageCreate(dispatch)) => {
                let dispatch_id = dispatch.dispatch_id.clone();
                self.deliver(outbound, &dispatch_id, move || {
                    let sender = Arc::clone(sender);
                    let dispatch = dispatch.clone();
                    async move {
                        let Delivery {
                            message_id,
                            thread_id,
                        } = sender.send_message(*dispatch).await?;
                        Ok((message_id, thread_id))
                    }
                })
                .await
            }
            Some(pb::server_frame::Body::MessageEdit(dispatch)) => {
                let dispatch_id = dispatch.dispatch_id.clone();
                self.deliver(outbound, &dispatch_id, move || {
                    let sender = Arc::clone(sender);
                    let dispatch = dispatch.clone();
                    async move { Ok((sender.edit_message(dispatch).await?, None)) }
                })
                .await
            }
            Some(pb::server_frame::Body::MessageDelete(dispatch)) => {
                let dispatch_id = dispatch.dispatch_id.clone();
                self.deliver(outbound, &dispatch_id, move || {
                    let sender = Arc::clone(sender);
                    let dispatch = dispatch.clone();
                    async move { Ok((sender.delete_message(dispatch).await?, None)) }
                })
                .await
            }
            Some(pb::server_frame::Body::MessageRetitle(dispatch)) => {
                let dispatch_id = dispatch.dispatch_id.clone();
                self.deliver(outbound, &dispatch_id, move || {
                    let sender = Arc::clone(sender);
                    let dispatch = dispatch.clone();
                    async move { Ok((sender.retitle_thread(dispatch).await?, None)) }
                })
                .await
            }
            Some(pb::server_frame::Body::Reaction(dispatch)) => {
                let dispatch_id = dispatch.dispatch_id.clone();
                self.deliver(outbound, &dispatch_id, move || {
                    let sender = Arc::clone(sender);
                    let dispatch = dispatch.clone();
                    async move { Ok((sender.apply_reaction(dispatch).await?, None)) }
                })
                .await
            }
            Some(pb::server_frame::Body::DeliveryAck(dispatch)) => {
                let dispatch_id = dispatch.dispatch_id.clone();
                self.deliver(outbound, &dispatch_id, move || {
                    let sender = Arc::clone(sender);
                    let dispatch = dispatch.clone();
                    async move { Ok((sender.ack_delivery(dispatch).await?, None)) }
                })
                .await
            }
            Some(pb::server_frame::Body::SeenReceipt(dispatch)) => {
                let dispatch_id = dispatch.dispatch_id.clone();
                self.deliver(outbound, &dispatch_id, move || {
                    let sender = Arc::clone(sender);
                    let dispatch = dispatch.clone();
                    async move { Ok((sender.mark_seen(dispatch).await?, None)) }
                })
                .await
            }
            Some(pb::server_frame::Body::Rejected(rejected)) => {
                Err(SessionError::Rejected(rejected.reason))
            }
            None => Err(SessionError::EmptyFrame),
        }
    }

    /// Run `op` with bounded retry, then resolve the dispatch with the core: a
    /// successful send is acknowledged, a permanent failure is dead-lettered,
    /// and a transient failure is surfaced to the session loop, which drops
    /// the session so the core can redeliver the still-unacknowledged
    /// dispatch.
    async fn deliver<F, Fut>(
        &self,
        outbound: &mpsc::Sender<pb::ClientFrame>,
        dispatch_id: &str,
        op: F,
    ) -> Result<(), SessionError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<(String, Option<String>), Failure>>,
    {
        match self.send_with_retry(dispatch_id, op).await {
            Ok((message_id, thread_id)) => {
                let acknowledgement = pb::ClientFrame {
                    body: Some(pb::client_frame::Body::Acknowledgement(
                        pb::Acknowledgement {
                            dispatch_id: dispatch_id.to_owned(),
                            platform_message_id: message_id.clone(),
                            thread_id,
                        },
                    )),
                };
                outbound
                    .send(acknowledgement)
                    .await
                    .map_err(|_| SessionError::StreamClosed)?;
                debug!(dispatch_id, platform_message_id = %message_id, "dispatch delivered and acknowledged");
                Ok(())
            }
            Err(failure) if self.is_permanent(&failure) => {
                error!(dispatch_id, %failure, "dispatch permanently failed, dead-lettering");
                let report = pb::ClientFrame {
                    body: Some(pb::client_frame::Body::DispatchFailed(pb::DispatchFailed {
                        dispatch_id: dispatch_id.to_owned(),
                        reason: failure.to_string(),
                    })),
                };
                outbound
                    .send(report)
                    .await
                    .map_err(|_| SessionError::StreamClosed)
            }
            Err(failure) => Err(SessionError::Delivery {
                dispatch_id: dispatch_id.to_owned(),
                reason: failure.to_string(),
            }),
        }
    }

    /// Retry a transient dispatch op a bounded number of times before
    /// surfacing the failure to the caller.
    async fn send_with_retry<F, Fut>(
        &self,
        dispatch_id: &str,
        op: F,
    ) -> Result<(String, Option<String>), Failure>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<(String, Option<String>), Failure>>,
    {
        let mut last_failure = None;
        for attempt in 1..=MAX_SEND_ATTEMPTS {
            if attempt > 1 {
                tokio::time::sleep(SEND_RETRY_DELAY).await;
            }
            match op().await {
                Ok(delivery) => return Ok(delivery),
                Err(failure) => {
                    warn!(dispatch_id, %failure, attempt, "dispatch send attempt failed");
                    if self.is_permanent(&failure) {
                        return Err(failure);
                    }
                    last_failure = Some(failure);
                }
            }
        }
        Err(last_failure.expect("at least one attempt ran"))
    }
}

fn core_uri(address: &str) -> Result<Uri, tonic::codegen::http::uri::InvalidUri> {
    if address.contains("://") {
        address.parse()
    } else {
        format!("http://{address}").parse()
    }
}
