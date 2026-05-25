use crate::{sessions::SessionRegistry, Engine, EngineError};
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};
use wuphf_proto::v1::{
    connector_server::Connector, connector_to_core, core_to_connector, ConnectorToCore,
    CoreToConnector, RegistrationResult,
};

type OutputStream =
    Pin<Box<dyn Stream<Item = Result<CoreToConnector, Status>> + Send + Sync + 'static>>;
#[derive(Clone)]
pub struct ConnectorService {
    engine: Engine,
    sessions: SessionRegistry,
}

impl ConnectorService {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            sessions: SessionRegistry::default(),
        }
    }

    async fn send_pending(
        &self,
        connector_id: &str,
        allow_early_retry: bool,
    ) -> Result<(), Status> {
        let sender = self.sessions.sender(connector_id).await;
        let Some(sender) = sender else {
            return Ok(());
        };
        for dispatch in self
            .engine
            .claim_dispatches(connector_id, allow_early_retry)
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
            self.schedule_wakeup(
                connector_id.to_string(),
                self.engine
                    .acknowledgement_timeout()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            );
        }
        Ok(())
    }

    fn schedule_wakeup(&self, connector_id: String, delay_ms: u64) {
        let service = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            if let Err(status) = service.send_pending(&connector_id, false).await {
                tracing::warn!(%connector_id, error = %status, "could not retry dispatch");
            }
        });
    }

    async fn handle_operation(
        &self,
        connector_id: &str,
        message: ConnectorToCore,
    ) -> Result<(), Status> {
        match message.operation {
            Some(connector_to_core::Operation::CreateMessage(message)) => {
                let result = self
                    .engine
                    .ingest(connector_id, message)
                    .await
                    .map_err(to_status)?;
                for target in result.target_connectors {
                    if let Err(status) = self.send_pending(&target, false).await {
                        tracing::warn!(%target, error = %status, "could not deliver dispatch");
                    }
                }
                Ok(())
            }
            Some(connector_to_core::Operation::DispatchSucceeded(outcome)) => self
                .engine
                .acknowledge(
                    connector_id,
                    &outcome.dispatch_id,
                    &outcome.platform_message_id,
                )
                .await
                .map_err(to_status),
            Some(connector_to_core::Operation::DispatchFailed(outcome)) => {
                match self
                    .engine
                    .fail(
                        connector_id,
                        &outcome.dispatch_id,
                        &outcome.reason,
                        outcome.permanent,
                    )
                    .await
                {
                    Ok(Some(delay)) => {
                        self.schedule_wakeup(
                            connector_id.to_string(),
                            delay.as_millis().try_into().unwrap_or(u64::MAX),
                        );
                        Ok(())
                    }
                    Ok(None) => Ok(()),
                    Err(error) => Err(to_status(error)),
                }
            }
            Some(connector_to_core::Operation::Register(_)) => {
                Err(Status::invalid_argument("connector already registered"))
            }
            None => Err(Status::invalid_argument("empty connector operation")),
        }
    }

    async fn is_active_session(&self, connector_id: &str, session_id: &str) -> bool {
        self.sessions.is_active(connector_id, session_id).await
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
        if !self
            .engine
            .authenticate_connector(&register.connector_id, &register.shared_token)
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
        let session = self
            .sessions
            .register(connector_id.clone(), sender.clone())
            .await;
        if let Some(prior_sender) = session.replaced_sender {
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
        self.send_pending(&connector_id, true).await?;

        let service = self.clone();
        let session_id = session.id;
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
                    Ok(message) => service.handle_operation(&connector_id, message).await,
                    Err(status) => Err(status),
                };
                if let Err(status) = result {
                    let _ = sender.send(Err(status)).await;
                    break;
                }
                if service.send_pending(&connector_id, false).await.is_err() {
                    break;
                }
            }
            service
                .sessions
                .remove_if_active(&connector_id, &session_id)
                .await;
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
