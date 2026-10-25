use crate::RuntimeChannels;
use std::collections::HashMap;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use wuphf_model::{Connector, ConnectorId, ConnectorInput, ConnectorOutput, Dispatch, DispatchId};

/// One registered connector session, as the transport holds it. The session
/// counts as live until `output` is dropped, so a transport drops it as soon as
/// the connector's stream ends.
pub struct ConnectorConnection {
    pub input: mpsc::Sender<ConnectorInput>,
    pub output: mpsc::Receiver<ConnectorOutput>,
}

/// Why the hub refused a connector session.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistrationError {
    #[error("connector registration does not match configuration")]
    Mismatch,
    #[error("a session for this connector is still live")]
    AlreadyLive,
    #[error("connector hub is stopped")]
    HubStopped,
}

#[derive(Clone)]
pub struct ConnectorHubHandle {
    connect_tx: mpsc::Sender<ConnectRequest>,
}

impl ConnectorHubHandle {
    pub async fn connect(
        &self,
        registration: Connector,
    ) -> Result<ConnectorConnection, RegistrationError> {
        let (input_tx, input_rx) = mpsc::channel(32);
        let (output_tx, output_rx) = mpsc::channel(32);
        let (result_tx, result_rx) = oneshot::channel();
        self.connect_tx
            .send(ConnectRequest {
                registration,
                input_rx,
                output_tx,
                result_tx,
            })
            .await
            .map_err(|_| RegistrationError::HubStopped)?;
        result_rx
            .await
            .map_err(|_| RegistrationError::HubStopped)??;
        Ok(ConnectorConnection {
            input: input_tx,
            output: output_rx,
        })
    }
}

/// Input from a session, tagged with the session's generation so input from a
/// session that has since been replaced can be told apart and dropped.
type TaggedInput = (ConnectorId, u64, ConnectorInput);

pub struct ConnectorHub {
    runtime: RuntimeChannels,
    connect_rx: mpsc::Receiver<ConnectRequest>,
    inbound_tx: mpsc::Sender<TaggedInput>,
    inbound_rx: mpsc::Receiver<TaggedInput>,
    allowed: HashMap<ConnectorId, Connector>,
    sessions: HashMap<ConnectorId, Session>,
    assigned: HashMap<DispatchId, (ConnectorId, Dispatch)>,
    held: Vec<Dispatch>,
    next_session: u64,
}

struct Session {
    output: mpsc::Sender<ConnectorOutput>,
    generation: u64,
}

struct ConnectRequest {
    registration: Connector,
    input_rx: mpsc::Receiver<ConnectorInput>,
    output_tx: mpsc::Sender<ConnectorOutput>,
    result_tx: oneshot::Sender<Result<(), RegistrationError>>,
}

impl ConnectorHub {
    pub fn new(runtime: RuntimeChannels, allowed: Vec<Connector>) -> (Self, ConnectorHubHandle) {
        let (connect_tx, connect_rx) = mpsc::channel(16);
        let (inbound_tx, inbound_rx) = mpsc::channel(64);
        (
            Self {
                runtime,
                connect_rx,
                inbound_tx,
                inbound_rx,
                allowed: allowed
                    .into_iter()
                    .map(|registration| (registration.id.clone(), registration))
                    .collect(),
                sessions: HashMap::new(),
                assigned: HashMap::new(),
                held: Vec::new(),
                next_session: 0,
            },
            ConnectorHubHandle { connect_tx },
        )
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(request) = self.connect_rx.recv() => self.register(request).await,
                Some((connector_id, generation, input)) = self.inbound_rx.recv() => {
                    self.accept_input(connector_id, generation, input).await;
                },
                dispatch = self.runtime.dispatches.recv() => match dispatch {
                    Some(dispatch) => self.deliver_or_hold(dispatch).await,
                    None => break,
                }
            }
        }
    }

    async fn register(&mut self, request: ConnectRequest) {
        let connector_id = request.registration.id.clone();
        if self.allowed.get(&connector_id) != Some(&request.registration) {
            warn!(
                connector = %connector_id.0,
                "rejecting connector session: registration does not match configuration",
            );
            let _ = request.result_tx.send(Err(RegistrationError::Mismatch));
            return;
        }
        // A second session for a connector whose session is still live is
        // refused rather than taking over: the live one keeps its dispatches.
        // A restarted connector gets in once the old stream has ended.
        if self.live_session(&connector_id).is_some() {
            warn!(
                connector = %connector_id.0,
                "rejecting connector session: a session for this connector is still live",
            );
            let _ = request.result_tx.send(Err(RegistrationError::AlreadyLive));
            return;
        }
        info!(connector = %connector_id.0, "connector session registered");
        let generation = self.next_session;
        self.next_session += 1;
        self.sessions.insert(
            connector_id.clone(),
            Session {
                output: request.output_tx,
                generation,
            },
        );
        let inbound_tx = self.inbound_tx.clone();
        let input_connector_id = connector_id.clone();
        tokio::spawn(async move {
            let mut input_rx = request.input_rx;
            while let Some(input) = input_rx.recv().await {
                if inbound_tx
                    .send((input_connector_id.clone(), generation, input))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        let _ = request.result_tx.send(Ok(()));

        let unacknowledged: Vec<_> = self
            .assigned
            .values()
            .filter(|(owner, _)| *owner == connector_id)
            .map(|(_, dispatch)| dispatch.clone())
            .collect();
        if !unacknowledged.is_empty() {
            info!(
                connector = %connector_id.0,
                count = unacknowledged.len(),
                "redelivering dispatches unacknowledged by the previous session",
            );
        }
        for dispatch in unacknowledged {
            self.deliver_or_hold(dispatch).await;
        }
        let pending = std::mem::take(&mut self.held);
        for dispatch in pending {
            self.deliver_or_hold(dispatch).await;
        }
    }

    /// The connector's current session, if its transport still holds it.
    fn live_session(&self, connector_id: &ConnectorId) -> Option<&Session> {
        self.sessions
            .get(connector_id)
            .filter(|session| !session.output.is_closed())
    }

    async fn accept_input(
        &mut self,
        connector_id: ConnectorId,
        generation: u64,
        input: ConnectorInput,
    ) {
        if self
            .sessions
            .get(&connector_id)
            .is_none_or(|session| session.generation != generation)
        {
            debug!(connector = %connector_id.0, "dropping input from a session that is no longer registered");
            return;
        }
        match input {
            ConnectorInput::Work(mut work) => {
                // The session is the authority on which connector spoke. Work
                // shares one ordered queue per connector, so a notice or
                // migration lands after whatever the connector already reported.
                work.set_connector(connector_id);
                let _ = self.runtime.inbound.send(*work).await;
            }
            ConnectorInput::Acknowledgement(acknowledgement) => {
                if self
                    .assigned
                    .get(&acknowledgement.dispatch_id)
                    .is_some_and(|(owner, _)| *owner == connector_id)
                {
                    self.assigned.remove(&acknowledgement.dispatch_id);
                    let _ = self.runtime.acknowledgements.send(acknowledgement).await;
                } else {
                    self.reject(
                        &connector_id,
                        "dispatch acknowledgement is not assigned to session",
                    )
                    .await;
                }
            }
            ConnectorInput::DispatchFailed(failure) => {
                if self
                    .assigned
                    .get(&failure.dispatch_id)
                    .is_some_and(|(owner, _)| *owner == connector_id)
                {
                    self.assigned.remove(&failure.dispatch_id);
                    let _ = self.runtime.failures.send(failure).await;
                } else {
                    self.reject(&connector_id, "dispatch failure is not assigned to session")
                        .await;
                }
            }
        }
    }

    async fn deliver_or_hold(&mut self, dispatch: Dispatch) {
        let connector_id = dispatch.to.connector_id.clone();
        let Some(session) = self.sessions.get(&connector_id) else {
            debug!(
                connector = %connector_id.0,
                dispatch_id = %dispatch.id,
                "holding dispatch until target connector connects",
            );
            self.held.push(dispatch);
            return;
        };
        if session
            .output
            .send(ConnectorOutput::Dispatch(Box::new(dispatch.clone())))
            .await
            .is_ok()
        {
            self.assigned
                .insert(dispatch.id, (connector_id, dispatch.clone()));
        } else {
            self.held.push(dispatch);
        }
    }

    async fn reject(&self, connector_id: &ConnectorId, reason: &str) {
        warn!(connector = %connector_id.0, reason, "rejecting connector input");
        if let Some(session) = self.sessions.get(connector_id) {
            let _ = session
                .output
                .send(ConnectorOutput::Rejected {
                    reason: reason.to_owned(),
                })
                .await;
        }
    }
}
