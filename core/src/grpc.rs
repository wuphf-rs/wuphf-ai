use crate::{Engine, EngineError};
use std::{collections::HashMap, pin::Pin, sync::Arc};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;
use wuphf_proto::v1::{
    connector_server::Connector, connector_to_core, core_to_connector, ConnectorToCore,
    CoreToConnector, RegistrationResult,
};

type OutputStream =
    Pin<Box<dyn Stream<Item = Result<CoreToConnector, Status>> + Send + Sync + 'static>>;
type Sender = mpsc::Sender<Result<CoreToConnector, Status>>;

#[derive(Clone)]
pub struct ConnectorService {
    engine: Engine,
    sessions: Arc<Mutex<HashMap<String, (String, Sender)>>>,
}

impl ConnectorService {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn send_pending(&self, connector_id: &str) -> Result<(), Status> {
        let sender = self
            .sessions
            .lock()
            .await
            .get(connector_id)
            .map(|(_, sender)| sender.clone());
        let Some(sender) = sender else {
            return Ok(());
        };
        for dispatch in self
            .engine
            .eligible_dispatches(connector_id)
            .await
            .map_err(to_status)?
        {
            if sender
                .send(Ok(CoreToConnector {
                    operation: Some(core_to_connector::Operation::DispatchCreateMessage(
                        dispatch,
                    )),
                }))
                .await
                .is_err()
            {
                break;
            }
        }
        Ok(())
    }

    async fn is_active_session(&self, connector_id: &str, session_id: &str) -> bool {
        self.sessions
            .lock()
            .await
            .get(connector_id)
            .is_some_and(|(current_id, _)| current_id == session_id)
    }
}

#[tonic::async_trait]
impl Connector for ConnectorService {
    type ConnectorStreamStream = OutputStream;

    async fn connector_stream(
        &self,
        request: Request<Streaming<ConnectorToCore>>,
    ) -> Result<Response<Self::ConnectorStreamStream>, Status> {
        let mut input = request.into_inner();
        let registration = input
            .next()
            .await
            .ok_or_else(|| Status::unauthenticated("registration is required"))??;
        let connector_to_core::Operation::Register(register) = registration
            .operation
            .ok_or_else(|| Status::unauthenticated("registration is required"))?
        else {
            return Err(Status::unauthenticated(
                "first operation must be registration",
            ));
        };

        let (sender, receiver) = mpsc::channel(32);
        if self.engine.config.connector_token(&register.connector_id)
            != Some(register.shared_token.as_str())
        {
            sender
                .send(Ok(CoreToConnector {
                    operation: Some(core_to_connector::Operation::RegistrationResult(
                        RegistrationResult {
                            accepted: false,
                            message: "invalid connector credentials".into(),
                        },
                    )),
                }))
                .await
                .map_err(|_| Status::internal("registration output stream closed"))?;
            return Ok(Response::new(Box::pin(ReceiverStream::new(receiver))));
        }

        let connector_id = register.connector_id;
        let session_id = Uuid::new_v4().to_string();
        let replaced = self
            .sessions
            .lock()
            .await
            .insert(connector_id.clone(), (session_id.clone(), sender.clone()));
        if let Some((_, prior_sender)) = replaced {
            let _ = prior_sender
                .send(Err(Status::aborted(
                    "connector session replaced by a newer registration",
                )))
                .await;
        }
        sender
            .send(Ok(CoreToConnector {
                operation: Some(core_to_connector::Operation::RegistrationResult(
                    RegistrationResult {
                        accepted: true,
                        message: "registered".into(),
                    },
                )),
            }))
            .await
            .map_err(|_| Status::internal("registration output stream closed"))?;
        self.send_pending(&connector_id).await?;

        let service = self.clone();
        tokio::spawn(async move {
            while let Some(operation) = input.next().await {
                if !service.is_active_session(&connector_id, &session_id).await {
                    let _ = sender
                        .send(Err(Status::aborted(
                            "connector session is no longer active",
                        )))
                        .await;
                    break;
                }
                let result = match operation {
                    Ok(message) => match message.operation {
                        Some(connector_to_core::Operation::CreateMessage(message)) => {
                            match service.engine.ingest(&connector_id, message).await {
                                Ok(result) => {
                                    for target in result.target_connectors {
                                        if let Err(status) = service.send_pending(&target).await {
                                            tracing::warn!(%target, error = %status, "could not deliver dispatch");
                                        }
                                    }
                                    Ok(())
                                }
                                Err(error) => Err(to_status(error)),
                            }
                        }
                        Some(connector_to_core::Operation::DispatchSucceeded(outcome)) => service
                            .engine
                            .acknowledge(
                                &connector_id,
                                &outcome.dispatch_id,
                                &outcome.platform_message_id,
                            )
                            .await
                            .map_err(to_status),
                        Some(connector_to_core::Operation::DispatchFailed(outcome)) => service
                            .engine
                            .fail(&connector_id, &outcome.dispatch_id, &outcome.reason)
                            .await
                            .map_err(to_status),
                        Some(connector_to_core::Operation::Register(_)) => {
                            Err(Status::invalid_argument("connector already registered"))
                        }
                        None => Err(Status::invalid_argument("empty connector operation")),
                    },
                    Err(status) => Err(status),
                };
                if let Err(status) = result {
                    let _ = sender.send(Err(status)).await;
                    break;
                }
                if service.send_pending(&connector_id).await.is_err() {
                    break;
                }
            }
            let mut sessions = service.sessions.lock().await;
            if sessions
                .get(&connector_id)
                .is_some_and(|(current_id, _)| current_id == &session_id)
            {
                sessions.remove(&connector_id);
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

fn to_status(error: EngineError) -> Status {
    match error {
        EngineError::UnauthorizedEndpoint(_) | EngineError::UnauthorizedDispatch => {
            Status::permission_denied(error.to_string())
        }
        EngineError::UnknownReply(_) | EngineError::ConflictingAcknowledgement => {
            Status::invalid_argument(error.to_string())
        }
        EngineError::Database(_) | EngineError::Metadata(_) => Status::internal(error.to_string()),
    }
}
