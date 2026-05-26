use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use wuphf_core::{Core, CoreError};
use wuphf_model::{DispatchAcknowledgement, NormalizedEvent, StoredDispatch};
use wuphf_storage::Storage;

pub mod connector;
pub mod grpc;

pub struct Runtime<S> {
    core: Arc<Core<S>>,
    event_rx: mpsc::Receiver<NormalizedEvent>,
    acknowledgement_rx: mpsc::Receiver<DispatchAcknowledgement>,
    dispatch_tx: mpsc::Sender<StoredDispatch>,
}

pub struct RuntimeChannels {
    pub events: mpsc::Sender<NormalizedEvent>,
    pub acknowledgements: mpsc::Sender<DispatchAcknowledgement>,
    pub dispatches: mpsc::Receiver<StoredDispatch>,
}

impl<S> Runtime<S>
where
    S: Storage,
{
    pub fn channelled(core: Arc<Core<S>>, capacity: usize) -> (Self, RuntimeChannels) {
        let (event_tx, event_rx) = mpsc::channel(capacity);
        let (acknowledgement_tx, acknowledgement_rx) = mpsc::channel(capacity);
        let (dispatch_tx, dispatch_rx) = mpsc::channel(capacity);
        (
            Self {
                core,
                event_rx,
                acknowledgement_rx,
                dispatch_tx,
            },
            RuntimeChannels {
                events: event_tx,
                acknowledgements: acknowledgement_tx,
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
mod tests {
    use super::*;
    use crate::connector::ConnectorHub;
    use crate::grpc::GrpcConnectorService;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
    use tonic::transport::Server;
    use wuphf_model::{
        ConnectorId, ConnectorInput, ConnectorOutput, ConnectorRegistration, DispatchPayload,
        Endpoint, EndpointId, EventPayload, GroupId, InboundMessage, Platform, ReplicationGroup,
    };
    use wuphf_proto::connector as pb;
    use wuphf_proto::connector::connector_transport_client::ConnectorTransportClient;
    use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
    use wuphf_storage::{InMemoryStorage, Storage};

    #[tokio::test]
    async fn transports_ready_dispatches_over_tokio_channels() {
        let storage = Arc::new(InMemoryStorage::default());
        let source = EndpointId::new();
        let target = EndpointId::new();
        let group_id = GroupId::new();
        storage
            .put_endpoint(Endpoint {
                id: source,
                platform: Platform::WhatsApp,
                chat_id: "wa".into(),
                thread_id: None,
            })
            .await
            .unwrap();
        storage
            .put_endpoint(Endpoint {
                id: target,
                platform: Platform::Telegram,
                chat_id: "tg".into(),
                thread_id: Some("general".into()),
            })
            .await
            .unwrap();
        storage
            .put_group(ReplicationGroup {
                id: group_id,
                endpoints: vec![source, target],
            })
            .await
            .unwrap();

        let (runtime, mut channels) = Runtime::channelled(Arc::new(Core::new(storage)), 4);
        let task = tokio::spawn(runtime.run());
        channels
            .events
            .send(NormalizedEvent {
                origin_endpoint: source,
                group_id,
                payload: EventPayload::MessageCreate(InboundMessage {
                    platform_message_id: "wa-1".into(),
                    sender: "sender".into(),
                    participant: None,
                    text: Some("runtime event".into()),
                    attachments: vec![],
                    reply_to_platform_message_id: None,
                    timestamp_ms: 1,
                }),
            })
            .await
            .unwrap();
        assert_eq!(
            channels
                .dispatches
                .recv()
                .await
                .unwrap()
                .command
                .target_endpoint,
            target
        );
        drop(channels.events);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn acknowledgement_releases_reply_with_target_native_parent_id() {
        let storage = Arc::new(InMemoryStorage::default());
        let source = EndpointId::new();
        let target = EndpointId::new();
        let group_id = GroupId::new();
        storage
            .put_endpoint(Endpoint {
                id: source,
                platform: Platform::WhatsApp,
                chat_id: "wa".into(),
                thread_id: None,
            })
            .await
            .unwrap();
        storage
            .put_endpoint(Endpoint {
                id: target,
                platform: Platform::Telegram,
                chat_id: "tg".into(),
                thread_id: Some("topic".into()),
            })
            .await
            .unwrap();
        storage
            .put_group(ReplicationGroup {
                id: group_id,
                endpoints: vec![source, target],
            })
            .await
            .unwrap();

        let (runtime, mut channels) =
            Runtime::channelled(Arc::new(Core::new(Arc::clone(&storage))), 8);
        let task = tokio::spawn(runtime.run());
        channels
            .events
            .send(message_event(source, group_id, "wa-parent", None))
            .await
            .unwrap();
        let parent_dispatch = channels.dispatches.recv().await.unwrap();
        channels
            .events
            .send(message_event(
                source,
                group_id,
                "wa-reply",
                Some("wa-parent"),
            ))
            .await
            .unwrap();
        assert_eq!(storage.pending_dispatches().await.unwrap().len(), 1);

        channels
            .acknowledgements
            .send(DispatchAcknowledgement {
                dispatch_id: parent_dispatch.id,
                platform_message_id: "tg-parent".into(),
            })
            .await
            .unwrap();
        let reply_dispatch = channels.dispatches.recv().await.unwrap();
        let DispatchPayload::MessageCreate {
            reply_to_platform_message_id,
            ..
        } = reply_dispatch.command.payload;
        assert_eq!(reply_to_platform_message_id.as_deref(), Some("tg-parent"));
        drop(channels.events);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connector_sessions_route_dispatches_and_authorize_inputs() {
        let storage = Arc::new(InMemoryStorage::default());
        let source = EndpointId::new();
        let target = EndpointId::new();
        let group_id = GroupId::new();
        storage
            .put_endpoint(Endpoint {
                id: source,
                platform: Platform::WhatsApp,
                chat_id: "wa".into(),
                thread_id: None,
            })
            .await
            .unwrap();
        storage
            .put_endpoint(Endpoint {
                id: target,
                platform: Platform::Telegram,
                chat_id: "tg".into(),
                thread_id: Some("topic".into()),
            })
            .await
            .unwrap();
        storage
            .put_group(ReplicationGroup {
                id: group_id,
                endpoints: vec![source, target],
            })
            .await
            .unwrap();

        let (runtime, runtime_channels) = Runtime::channelled(Arc::new(Core::new(storage)), 8);
        let (hub, handle) = ConnectorHub::new(
            runtime_channels,
            vec![
                ConnectorRegistration {
                    id: ConnectorId("whatsapp".into()),
                    platform: Platform::WhatsApp,
                    endpoints: vec![source],
                },
                ConnectorRegistration {
                    id: ConnectorId("telegram".into()),
                    platform: Platform::Telegram,
                    endpoints: vec![target],
                },
            ],
        );
        let runtime_task = tokio::spawn(runtime.run());
        let hub_task = tokio::spawn(hub.run());
        let whatsapp = handle
            .connect(ConnectorRegistration {
                id: ConnectorId("whatsapp".into()),
                platform: Platform::WhatsApp,
                endpoints: vec![source],
            })
            .await
            .unwrap();
        let mut telegram = handle
            .connect(ConnectorRegistration {
                id: ConnectorId("telegram".into()),
                platform: Platform::Telegram,
                endpoints: vec![target],
            })
            .await
            .unwrap();
        assert!(
            handle
                .connect(ConnectorRegistration {
                    id: ConnectorId("rogue".into()),
                    platform: Platform::Telegram,
                    endpoints: vec![target],
                })
                .await
                .is_err()
        );

        telegram
            .input
            .send(ConnectorInput::Event(message_event(
                source, group_id, "forged", None,
            )))
            .await
            .unwrap();
        assert!(matches!(
            telegram.output.recv().await.unwrap(),
            ConnectorOutput::Rejected { .. }
        ));

        whatsapp
            .input
            .send(ConnectorInput::Event(message_event(
                source,
                group_id,
                "wa-parent",
                None,
            )))
            .await
            .unwrap();
        let ConnectorOutput::Dispatch(parent_dispatch) = telegram.output.recv().await.unwrap()
        else {
            panic!("Telegram should receive parent dispatch");
        };
        drop(telegram);
        let mut telegram = handle
            .connect(ConnectorRegistration {
                id: ConnectorId("telegram".into()),
                platform: Platform::Telegram,
                endpoints: vec![target],
            })
            .await
            .unwrap();
        let ConnectorOutput::Dispatch(redelivered_parent) = telegram.output.recv().await.unwrap()
        else {
            panic!("Telegram should receive unacknowledged dispatch after reconnect");
        };
        assert_eq!(redelivered_parent.id, parent_dispatch.id);
        whatsapp
            .input
            .send(ConnectorInput::Event(message_event(
                source,
                group_id,
                "wa-reply",
                Some("wa-parent"),
            )))
            .await
            .unwrap();
        telegram
            .input
            .send(ConnectorInput::Acknowledgement(
                wuphf_model::DispatchAcknowledgement {
                    dispatch_id: parent_dispatch.id,
                    platform_message_id: "tg-parent".into(),
                },
            ))
            .await
            .unwrap();
        let ConnectorOutput::Dispatch(reply_dispatch) = telegram.output.recv().await.unwrap()
        else {
            panic!("Telegram should receive released reply dispatch");
        };
        let reply_dispatch = *reply_dispatch;
        let DispatchPayload::MessageCreate {
            reply_to_platform_message_id,
            ..
        } = reply_dispatch.command.payload;
        assert_eq!(reply_to_platform_message_id.as_deref(), Some("tg-parent"));

        hub_task.abort();
        runtime_task.abort();
    }

    #[tokio::test]
    async fn grpc_transport_delivers_dispatch_to_registered_connector() {
        let storage = Arc::new(InMemoryStorage::default());
        let source = EndpointId::new();
        let target = EndpointId::new();
        let group_id = GroupId::new();
        storage
            .put_endpoint(Endpoint {
                id: source,
                platform: Platform::WhatsApp,
                chat_id: "wa".into(),
                thread_id: None,
            })
            .await
            .unwrap();
        storage
            .put_endpoint(Endpoint {
                id: target,
                platform: Platform::Telegram,
                chat_id: "tg".into(),
                thread_id: Some("topic".into()),
            })
            .await
            .unwrap();
        storage
            .put_group(ReplicationGroup {
                id: group_id,
                endpoints: vec![source, target],
            })
            .await
            .unwrap();

        let (runtime, runtime_channels) = Runtime::channelled(Arc::new(Core::new(storage)), 8);
        let (hub, handle) = ConnectorHub::new(
            runtime_channels,
            vec![
                ConnectorRegistration {
                    id: ConnectorId("whatsapp".into()),
                    platform: Platform::WhatsApp,
                    endpoints: vec![source],
                },
                ConnectorRegistration {
                    id: ConnectorId("telegram".into()),
                    platform: Platform::Telegram,
                    endpoints: vec![target],
                },
            ],
        );
        let runtime_task = tokio::spawn(runtime.run());
        let hub_task = tokio::spawn(hub.run());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(
            Server::builder()
                .add_service(ConnectorTransportServer::new(GrpcConnectorService::new(
                    handle,
                )))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        let uri = format!("http://{address}");

        let (telegram_tx, telegram_rx) = mpsc::channel(8);
        telegram_tx
            .send(registration_frame(
                "telegram",
                pb::Platform::Telegram,
                target,
            ))
            .await
            .unwrap();
        let mut telegram_output = ConnectorTransportClient::connect(uri.clone())
            .await
            .unwrap()
            .open_session(ReceiverStream::new(telegram_rx))
            .await
            .unwrap()
            .into_inner();
        let (whatsapp_tx, whatsapp_rx) = mpsc::channel(8);
        whatsapp_tx
            .send(registration_frame(
                "whatsapp",
                pb::Platform::Whatsapp,
                source,
            ))
            .await
            .unwrap();
        let mut whatsapp_client = ConnectorTransportClient::connect(uri).await.unwrap();
        let _whatsapp_output = whatsapp_client
            .open_session(ReceiverStream::new(whatsapp_rx))
            .await
            .unwrap()
            .into_inner();
        whatsapp_tx
            .send(pb::ClientFrame {
                body: Some(pb::client_frame::Body::MessageCreate(
                    pb::MessageCreateEvent {
                        origin_endpoint_id: source.to_string(),
                        group_id: group_id.to_string(),
                        platform_message_id: "wa-grpc".into(),
                        sender: "wire".into(),
                        participant: None,
                        text: Some("over grpc".into()),
                        attachments: vec![],
                        reply_to_platform_message_id: None,
                        timestamp_ms: 1,
                    },
                )),
            })
            .await
            .unwrap();
        let frame = telegram_output.message().await.unwrap().unwrap();
        let Some(pb::server_frame::Body::MessageCreate(dispatch)) = frame.body else {
            panic!("Telegram gRPC stream should receive message dispatch");
        };
        assert_eq!(dispatch.target_endpoint_id, target.to_string());
        assert_eq!(dispatch.text.as_deref(), Some("over grpc"));

        server_task.abort();
        hub_task.abort();
        runtime_task.abort();
    }

    fn registration_frame(
        connector_id: &str,
        platform: pb::Platform,
        endpoint_id: EndpointId,
    ) -> pb::ClientFrame {
        pb::ClientFrame {
            body: Some(pb::client_frame::Body::Registration(pb::Registration {
                connector_id: connector_id.into(),
                platform: platform.into(),
                endpoint_ids: vec![endpoint_id.to_string()],
            })),
        }
    }

    fn message_event(
        endpoint: EndpointId,
        group_id: GroupId,
        native_id: &str,
        reply_to: Option<&str>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            origin_endpoint: endpoint,
            group_id,
            payload: EventPayload::MessageCreate(InboundMessage {
                platform_message_id: native_id.into(),
                sender: "sender".into(),
                participant: None,
                text: Some("message".into()),
                attachments: vec![],
                reply_to_platform_message_id: reply_to.map(str::to_owned),
                timestamp_ms: 1,
            }),
        }
    }
}
