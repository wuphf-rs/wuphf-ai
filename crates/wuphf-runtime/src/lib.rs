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
mod tests {
    use super::*;
    use crate::connector::ConnectorHub;
    use crate::grpc::GrpcConnectorService;
    use crate::media::GrpcMediaService;
    use std::collections::HashMap;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
    use tonic::transport::Server;
    use wuphf_core::{ConnectorInfo, RoutingConfig};
    use wuphf_model::{
        ConnectorCapabilities, ConnectorId, ConnectorInput, ConnectorOutput, ConnectorRegistration,
        DispatchPayload, EventPayload, InboundMessage, Platform, Rule, RuleId, RuleSide, SideKind,
    };
    use wuphf_proto::connector as pb;
    use wuphf_proto::connector::connector_transport_client::ConnectorTransportClient;
    use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
    use wuphf_proto::connector::media_transport_client::MediaTransportClient;
    use wuphf_proto::connector::media_transport_server::MediaTransportServer;
    use wuphf_storage::InMemoryStorage;

    const WA: &str = "whatsapp-main";
    const TG: &str = "telegram-main";

    fn rules() -> Vec<Rule> {
        vec![Rule {
            id: RuleId("direct".into()),
            sides: vec![
                RuleSide {
                    connector: ConnectorId(WA.into()),
                    kind: SideKind::Single {
                        chat_id: "wa-chat".into(),
                        thread_id: None,
                    },
                },
                RuleSide {
                    connector: ConnectorId(TG.into()),
                    kind: SideKind::Single {
                        chat_id: "tg-chat".into(),
                        thread_id: Some("topic".into()),
                    },
                },
            ],
        }]
    }

    fn telegram_capabilities() -> ConnectorCapabilities {
        ConnectorCapabilities {
            supports_threads: true,
            supports_topic_create: true,
        }
    }

    fn connector_infos() -> HashMap<ConnectorId, ConnectorInfo> {
        HashMap::from([
            (
                ConnectorId(WA.into()),
                ConnectorInfo {
                    platform: Platform::WhatsApp,
                    capabilities: ConnectorCapabilities::default(),
                    delivery: Default::default(),
                },
            ),
            (
                ConnectorId(TG.into()),
                ConnectorInfo {
                    platform: Platform::Telegram,
                    capabilities: telegram_capabilities(),
                    delivery: Default::default(),
                },
            ),
        ])
    }

    fn registrations() -> Vec<ConnectorRegistration> {
        vec![
            ConnectorRegistration {
                id: ConnectorId(WA.into()),
                platform: Platform::WhatsApp,
                capabilities: ConnectorCapabilities::default(),
            },
            ConnectorRegistration {
                id: ConnectorId(TG.into()),
                platform: Platform::Telegram,
                capabilities: telegram_capabilities(),
            },
        ]
    }

    fn core() -> Arc<Core<InMemoryStorage>> {
        let storage = Arc::new(InMemoryStorage::default());
        let config = Arc::new(RoutingConfig::new(rules(), connector_infos()));
        Arc::new(Core::new(storage, config))
    }

    #[tokio::test]
    async fn transports_ready_dispatches_over_tokio_channels() {
        let (runtime, mut channels) = Runtime::channelled(core(), 4);
        let task = tokio::spawn(runtime.run());
        channels.events.send(wa_event("wa-1", None)).await.unwrap();
        let dispatch = channels.dispatches.recv().await.unwrap();
        assert_eq!(dispatch.command.target_connector, ConnectorId(TG.into()));
        assert_eq!(dispatch.command.target.address.chat_id, "tg-chat");
        drop(channels.events);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn acknowledgement_releases_reply_with_target_native_parent_id() {
        let (runtime, mut channels) = Runtime::channelled(core(), 8);
        let task = tokio::spawn(runtime.run());
        channels
            .events
            .send(wa_event("wa-parent", None))
            .await
            .unwrap();
        let parent_dispatch = channels.dispatches.recv().await.unwrap();
        channels
            .events
            .send(wa_event("wa-reply", Some("wa-parent")))
            .await
            .unwrap();

        channels
            .acknowledgements
            .send(DispatchAcknowledgement {
                dispatch_id: parent_dispatch.id,
                platform_message_id: "tg-parent".into(),
                thread_id: None,
            })
            .await
            .unwrap();
        let reply_dispatch = channels.dispatches.recv().await.unwrap();
        let DispatchPayload::MessageCreate { reply_context, .. } = reply_dispatch.command.payload
        else {
            panic!("released reply should be a message create");
        };
        let reply_context = reply_context.expect("reply context set on release");
        assert_eq!(reply_context.platform_message_id, "tg-parent");
        drop(channels.events);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connector_sessions_route_dispatches_and_authorize_inputs() {
        let (runtime, runtime_channels) = Runtime::channelled(core(), 8);
        let (hub, handle) = ConnectorHub::new(runtime_channels, registrations());
        let runtime_task = tokio::spawn(runtime.run());
        let hub_task = tokio::spawn(hub.run());
        let whatsapp = handle.connect(registrations()[0].clone()).await.unwrap();
        let mut telegram = handle.connect(registrations()[1].clone()).await.unwrap();
        assert!(
            handle
                .connect(ConnectorRegistration {
                    id: ConnectorId("rogue".into()),
                    platform: Platform::Telegram,
                    capabilities: telegram_capabilities(),
                })
                .await
                .is_err()
        );

        // An acknowledgement for a dispatch never assigned to the session is rejected.
        telegram
            .input
            .send(ConnectorInput::Acknowledgement(DispatchAcknowledgement {
                dispatch_id: wuphf_model::DispatchId::new(),
                platform_message_id: "ghost".into(),
                thread_id: None,
            }))
            .await
            .unwrap();
        assert!(matches!(
            telegram.output.recv().await.unwrap(),
            ConnectorOutput::Rejected { .. }
        ));

        whatsapp
            .input
            .send(ConnectorInput::Event(wa_event("wa-parent", None)))
            .await
            .unwrap();
        let ConnectorOutput::Dispatch(parent_dispatch) = telegram.output.recv().await.unwrap()
        else {
            panic!("Telegram should receive parent dispatch");
        };
        assert_eq!(parent_dispatch.command.target.address.chat_id, "tg-chat");
        drop(telegram);
        let mut telegram = handle.connect(registrations()[1].clone()).await.unwrap();
        let ConnectorOutput::Dispatch(redelivered_parent) = telegram.output.recv().await.unwrap()
        else {
            panic!("Telegram should receive unacknowledged dispatch after reconnect");
        };
        assert_eq!(redelivered_parent.id, parent_dispatch.id);
        whatsapp
            .input
            .send(ConnectorInput::Event(wa_event(
                "wa-reply",
                Some("wa-parent"),
            )))
            .await
            .unwrap();
        telegram
            .input
            .send(ConnectorInput::Acknowledgement(DispatchAcknowledgement {
                dispatch_id: parent_dispatch.id,
                platform_message_id: "tg-parent".into(),
                thread_id: None,
            }))
            .await
            .unwrap();
        let ConnectorOutput::Dispatch(reply_dispatch) = telegram.output.recv().await.unwrap()
        else {
            panic!("Telegram should receive released reply dispatch");
        };
        let reply_dispatch = *reply_dispatch;
        let DispatchPayload::MessageCreate { reply_context, .. } = reply_dispatch.command.payload
        else {
            panic!("released reply should be a message create");
        };
        let reply_context = reply_context.expect("reply context set on release");
        assert_eq!(reply_context.platform_message_id, "tg-parent");

        hub_task.abort();
        runtime_task.abort();
    }

    #[tokio::test]
    async fn media_transport_stores_downloadable_bytes_prunes_orphans_and_enforces_limit() {
        let storage = Arc::new(InMemoryStorage::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            Server::builder()
                .add_service(MediaTransportServer::new(GrpcMediaService::new(
                    storage,
                    5,
                    std::time::Duration::ZERO,
                )))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        let uri = format!("http://{address}");
        let mut client = MediaTransportClient::connect(uri).await.unwrap();

        let uploaded = client
            .upload_media(tokio_stream::iter(vec![
                pb::UploadMediaFrame {
                    body: Some(pb::upload_media_frame::Body::Metadata(pb::MediaMetadata {
                        kind: pb::attachment::Kind::Image.into(),
                        file_name: Some("photo.jpg".into()),
                        mime_type: Some("image/jpeg".into()),
                        size_bytes: Some(5),
                    })),
                },
                pb::UploadMediaFrame {
                    body: Some(pb::upload_media_frame::Body::Chunk(b"hello".to_vec())),
                },
            ]))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(uploaded.file_name.as_deref(), Some("photo.jpg"));
        assert_eq!(uploaded.size_bytes, Some(5));
        let first_media_id = uploaded.media_id.clone();

        let mut download = client
            .download_media(pb::DownloadMediaRequest {
                media_id: uploaded.media_id,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            download.message().await.unwrap().unwrap().chunk,
            b"hello".to_vec()
        );

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        client
            .upload_media(tokio_stream::iter(vec![
                pb::UploadMediaFrame {
                    body: Some(pb::upload_media_frame::Body::Metadata(pb::MediaMetadata {
                        kind: pb::attachment::Kind::File.into(),
                        file_name: None,
                        mime_type: None,
                        size_bytes: Some(1),
                    })),
                },
                pb::UploadMediaFrame {
                    body: Some(pb::upload_media_frame::Body::Chunk(b"x".to_vec())),
                },
            ]))
            .await
            .unwrap();
        let error = client
            .download_media(pb::DownloadMediaRequest {
                media_id: first_media_id,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::NotFound);

        let error = client
            .upload_media(tokio_stream::iter(vec![
                pb::UploadMediaFrame {
                    body: Some(pb::upload_media_frame::Body::Metadata(pb::MediaMetadata {
                        kind: pb::attachment::Kind::File.into(),
                        file_name: None,
                        mime_type: None,
                        size_bytes: None,
                    })),
                },
                pb::UploadMediaFrame {
                    body: Some(pb::upload_media_frame::Body::Chunk(b"123456".to_vec())),
                },
            ]))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::ResourceExhausted);
        server.abort();
    }

    #[tokio::test]
    async fn grpc_transport_delivers_dispatch_to_registered_connector() {
        let (runtime, runtime_channels) = Runtime::channelled(core(), 8);
        let (hub, handle) = ConnectorHub::new(runtime_channels, registrations());
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
                TG,
                pb::Platform::Telegram,
                Some(pb::Capabilities {
                    supports_threads: true,
                    supports_topic_create: true,
                }),
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
            .send(registration_frame(WA, pb::Platform::Whatsapp, None))
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
                        chat_id: "wa-chat".into(),
                        thread_id: None,
                        chat_title: None,
                        platform_message_id: "wa-grpc".into(),
                        sender: "wire".into(),
                        participant: None,
                        text: Some("over grpc".into()),
                        attachments: vec![],
                        reply_to_platform_message_id: None,
                        timestamp_ms: 1,
                        sender_name: None,
                    },
                )),
            })
            .await
            .unwrap();
        let frame = telegram_output.message().await.unwrap().unwrap();
        let Some(pb::server_frame::Body::MessageCreate(dispatch)) = frame.body else {
            panic!("Telegram gRPC stream should receive message dispatch");
        };
        assert_eq!(dispatch.target_chat_id, "tg-chat");
        assert_eq!(dispatch.target_thread_id.as_deref(), Some("topic"));
        assert_eq!(dispatch.text.as_deref(), Some("over grpc"));

        server_task.abort();
        hub_task.abort();
        runtime_task.abort();
    }

    fn registration_frame(
        connector_id: &str,
        platform: pb::Platform,
        capabilities: Option<pb::Capabilities>,
    ) -> pb::ClientFrame {
        pb::ClientFrame {
            body: Some(pb::client_frame::Body::Registration(pb::Registration {
                connector_id: connector_id.into(),
                platform: platform.into(),
                capabilities,
            })),
        }
    }

    fn wa_event(native_id: &str, reply_to: Option<&str>) -> NormalizedEvent {
        NormalizedEvent {
            source: wuphf_model::SourceAddress {
                connector_id: ConnectorId(WA.into()),
                address: PlatformAddress::new("wa-chat", None),
                chat_title: None,
            },
            payload: EventPayload::MessageCreate(InboundMessage {
                platform_message_id: native_id.into(),
                sender: "sender".into(),
                participant: None,
                sender_name: None,
                text: Some("message".into()),
                attachments: vec![],
                reply_to_platform_message_id: reply_to.map(str::to_owned),
                timestamp_ms: 1,
            }),
        }
    }
}
