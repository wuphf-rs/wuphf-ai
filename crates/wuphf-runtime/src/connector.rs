use crate::RuntimeChannels;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use wuphf_model::InboundWork;
use wuphf_model::{
    ConnectorId, ConnectorInput, ConnectorOutput, ConnectorRegistration, DispatchId, Dispatch,
};

pub struct ConnectorConnection {
    pub input: mpsc::Sender<ConnectorInput>,
    pub output: mpsc::Receiver<ConnectorOutput>,
}

#[derive(Clone)]
pub struct ConnectorHubHandle {
    connect_tx: mpsc::Sender<ConnectRequest>,
}

impl ConnectorHubHandle {
    pub async fn connect(
        &self,
        registration: ConnectorRegistration,
    ) -> Result<ConnectorConnection, String> {
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
            .map_err(|_| "connector hub is stopped".to_owned())?;
        result_rx
            .await
            .map_err(|_| "connector hub stopped before registration".to_owned())??;
        Ok(ConnectorConnection {
            input: input_tx,
            output: output_rx,
        })
    }
}

pub struct ConnectorHub {
    runtime: RuntimeChannels,
    connect_rx: mpsc::Receiver<ConnectRequest>,
    inbound_tx: mpsc::Sender<(ConnectorId, ConnectorInput)>,
    inbound_rx: mpsc::Receiver<(ConnectorId, ConnectorInput)>,
    allowed: HashMap<ConnectorId, ConnectorRegistration>,
    sessions: HashMap<ConnectorId, Session>,
    assigned: HashMap<DispatchId, (ConnectorId, Dispatch)>,
    held: Vec<Dispatch>,
}

struct Session {
    output: mpsc::Sender<ConnectorOutput>,
}

struct ConnectRequest {
    registration: ConnectorRegistration,
    input_rx: mpsc::Receiver<ConnectorInput>,
    output_tx: mpsc::Sender<ConnectorOutput>,
    result_tx: oneshot::Sender<Result<(), String>>,
}

impl ConnectorHub {
    pub fn new(
        runtime: RuntimeChannels,
        allowed: Vec<ConnectorRegistration>,
    ) -> (Self, ConnectorHubHandle) {
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
            },
            ConnectorHubHandle { connect_tx },
        )
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(request) = self.connect_rx.recv() => self.register(request).await,
                Some((connector_id, input)) = self.inbound_rx.recv() => {
                    self.accept_input(connector_id, input).await;
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
            let _ = request.result_tx.send(Err(
                "connector registration does not match configuration".to_owned(),
            ));
            return;
        }
        info!(connector = %connector_id.0, "connector session registered");
        self.sessions.insert(
            connector_id.clone(),
            Session {
                output: request.output_tx,
            },
        );
        let inbound_tx = self.inbound_tx.clone();
        let input_connector_id = connector_id.clone();
        tokio::spawn(async move {
            let mut input_rx = request.input_rx;
            while let Some(input) = input_rx.recv().await {
                if inbound_tx
                    .send((input_connector_id.clone(), input))
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

    async fn accept_input(&mut self, connector_id: ConnectorId, input: ConnectorInput) {
        if !self.sessions.contains_key(&connector_id) {
            debug!(connector = %connector_id.0, "dropping input from a session that is no longer registered");
            return;
        }
        match input {
            ConnectorInput::Event(mut event) => {
                // The session is the authority on which connector spoke; stamp it
                // so a connector cannot present itself as another.
                event.source.connector_id = connector_id;
                let _ = self.runtime.inbound.send(InboundWork::Event(event)).await;
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
            ConnectorInput::OwnerNotice { kind, detail, .. } => {
                // Onto the same ordered queue as this connector's events, so a
                // notice lands after whatever it had already reported rather
                // than overtaking it.
                let _ = self
                    .runtime
                    .inbound
                    .send(InboundWork::Notice {
                        connector_id,
                        kind,
                        detail,
                    })
                    .await;
            }
            ConnectorInput::IdentityMigrated { old, new, .. } => {
                let _ = self
                    .runtime
                    .inbound
                    .send(InboundWork::Migration {
                        connector_id,
                        old,
                        new,
                    })
                    .await;
            }
            ConnectorInput::DispatchFailed {
                dispatch_id,
                reason,
            } => {
                if self
                    .assigned
                    .get(&dispatch_id)
                    .is_some_and(|(owner, _)| *owner == connector_id)
                {
                    self.assigned.remove(&dispatch_id);
                    let _ = self.runtime.failures.send((dispatch_id, reason)).await;
                } else {
                    self.reject(&connector_id, "dispatch failure is not assigned to session")
                        .await;
                }
            }
        }
    }

    async fn deliver_or_hold(&mut self, dispatch: Dispatch) {
        let connector_id = dispatch.command.target_connector.clone();
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
