use super::*;
use crate::connector::ConnectorHub;
use crate::grpc::GrpcConnectorService;
use crate::media::GrpcMediaService;
use std::collections::HashMap;
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Server;
use wuphf_core::RoutingConfig;
use wuphf_model::{
    ConnectorCapabilities, ConnectorId, ConnectorInfo, ConnectorInput, ConnectorOutput,
    ConnectorRegistration, DispatchPayload, EventPayload, InboundMessage, IngestOutcome, Platform,
    PlatformAddress, RoutingRule, RoutingRuleFlow, RuleChat, RuleChatsFilter, RuleId,
};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_client::ConnectorTransportClient;
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_proto::connector::media_transport_client::MediaTransportClient;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_storage::InMemoryStorage;
use wuphf_storage::Storage;

const WA: &str = "whatsapp-main";
const TG: &str = "telegram-main";

fn rules() -> Vec<RoutingRule> {
    vec![RoutingRule {
        id: RuleId("direct".into()),
        flow: RoutingRuleFlow::OneToOne {
            source_connector: ConnectorId(WA.into()),
            source_chat: RuleChat {
                chat_id: "wa-chat".into(),
                thread_id: None,
            },
            destination_connector: ConnectorId(TG.into()),
            destination_chat: RuleChat {
                chat_id: "tg-chat".into(),
                thread_id: Some("topic".into()),
            },
        },
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
            },
        ),
        (
            ConnectorId(TG.into()),
            ConnectorInfo {
                platform: Platform::Telegram,
                capabilities: telegram_capabilities(),
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
    core_over(Arc::new(InMemoryStorage::default()))
}

/// Build a core over caller-owned storage, so a test can construct a second
/// core/runtime over the same storage to simulate a process restart.
fn core_over(storage: Arc<InMemoryStorage>) -> Arc<Core<InMemoryStorage>> {
    let config = Arc::new(RoutingConfig::new(rules(), connector_infos()));
    Arc::new(Core::new(storage, config))
}

/// A many-to-one mirror rule whose destination thread must be provisioned on
/// first send — the flow that stashes concurrent messages behind provisioning.
fn mirror_rules() -> Vec<RoutingRule> {
    vec![RoutingRule {
        id: RuleId("mirror".into()),
        flow: RoutingRuleFlow::ManyToOne {
            source_connector: ConnectorId(WA.into()),
            source_chats_filter: RuleChatsFilter::Only {
                chats: vec![RuleChat {
                    chat_id: "wa-group".into(),
                    thread_id: None,
                }],
            },
            destination_connector: ConnectorId(TG.into()),
            destination_chat: RuleChat {
                chat_id: "tg-group".into(),
                thread_id: None,
            },
        },
    }]
}

fn mirror_core(storage: Arc<InMemoryStorage>) -> Arc<Core<InMemoryStorage>> {
    let config = Arc::new(RoutingConfig::new(mirror_rules(), connector_infos()));
    Arc::new(Core::new(storage, config))
}

fn wa_group_event(native_id: &str) -> NormalizedEvent {
    source_event(
        ConnectorId(WA.into()),
        PlatformAddress::new("wa-group", None),
        native_id,
        None,
    )
}

#[tokio::test]
async fn transports_ready_dispatches_over_tokio_channels() {
    let (runtime, mut channels, _observer) = Runtime::channelled(core(), 4);
    let task = tokio::spawn(runtime.run());
    send_event(&channels, wa_event("wa-1", None)).await;
    let dispatch = channels.dispatches.recv().await.unwrap();
    assert_eq!(dispatch.command.target_connector, ConnectorId(TG.into()));
    assert_eq!(dispatch.command.target.address.chat_id, "tg-chat");
    drop(channels.inbound);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn interleaved_connectors_route_through_independent_workers() {
    // Events from two different source connectors, interleaved on the shared
    // intake, must each be demuxed to their own ingest worker and routed to the
    // opposite side. A demux bug funnelling everything to one worker would drop
    // or misroute one of the two dispatches.
    let (runtime, mut channels, _observer) = Runtime::channelled(core(), 8);
    let task = tokio::spawn(runtime.run());
    send_event(&channels, wa_event("wa-1", None)).await;
    send_event(&channels, tg_event("tg-1", None)).await;

    let first = channels.dispatches.recv().await.unwrap();
    let second = channels.dispatches.recv().await.unwrap();
    let targets = [
        first.command.target_connector,
        second.command.target_connector,
    ];
    assert!(
        targets.contains(&ConnectorId(WA.into())),
        "Telegram event should route to WhatsApp",
    );
    assert!(
        targets.contains(&ConnectorId(TG.into())),
        "WhatsApp event should route to Telegram",
    );
    drop(channels.inbound);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn migration_before_event_is_applied_first_on_one_connector() {
    // A connector sends an identity migration then a message under the new
    // address, in that frame order. Both travel one per-connector worker queue,
    // so the migration's alias is written before the message resolves: the
    // message under the migrated-to address routes to the existing endpoint
    // instead of being ignored as an unknown chat.
    let (runtime, mut channels, _observer) = Runtime::channelled(core(), 8);
    let task = tokio::spawn(runtime.run());
    // First sighting on the rule's WhatsApp chat mints its endpoint + alias.
    send_event(&channels, wa_event("wa-1", None)).await;
    let _ = channels.dispatches.recv().await.unwrap();

    // Rebind that chat to a new address, then immediately send under the new one.
    channels
        .inbound
        .send(InboundWork::Migration {
            connector_id: ConnectorId(WA.into()),
            old: PlatformAddress::new("wa-chat", None),
            new: PlatformAddress::new("wa-chat-migrated", None),
        })
        .await
        .unwrap();
    send_event(
        &channels,
        source_event(
            ConnectorId(WA.into()),
            PlatformAddress::new("wa-chat-migrated", None),
            "wa-2",
            None,
        ),
    )
    .await;

    // The message under the migrated address routes only if the migration was
    // applied first; otherwise it matches no rule and yields no dispatch.
    let dispatch = channels.dispatches.recv().await.unwrap();
    assert_eq!(dispatch.command.target_connector, ConnectorId(TG.into()));
    assert_eq!(dispatch.command.target.address.chat_id, "tg-chat");
    drop(channels.inbound);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn acknowledgement_releases_reply_with_target_native_parent_id() {
    let (runtime, mut channels, _observer) = Runtime::channelled(core(), 8);
    let task = tokio::spawn(runtime.run());
    send_event(&channels, wa_event("wa-parent", None)).await;
    let parent_dispatch = channels.dispatches.recv().await.unwrap();
    send_event(&channels, wa_event("wa-reply", Some("wa-parent"))).await;

    channels
        .acknowledgements
        .send(DispatchAcknowledgement {
            dispatch_id: parent_dispatch.id,
            platform_message_id: "tg-parent".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    // Acknowledging the parent also emits a delivery-ack to the origin, so skip
    // to the released reply create among the dispatches.
    let reply_context = loop {
        let dispatch = channels.dispatches.recv().await.unwrap();
        if let DispatchPayload::MessageCreate { reply_context, .. } = dispatch.command.payload {
            break reply_context.expect("reply context set on release");
        }
    };
    assert_eq!(reply_context.platform_message_id, "tg-parent");
    drop(channels.inbound);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn failed_dispatch_is_dead_lettered_and_not_redelivered() {
    let storage = Arc::new(InMemoryStorage::default());
    let (runtime, mut channels, _observer) =
        Runtime::channelled(core_over(Arc::clone(&storage)), 8);
    let task = tokio::spawn(runtime.run());
    send_event(&channels, wa_event("wa-1", None)).await;
    let dispatch = channels.dispatches.recv().await.unwrap();

    channels
        .failures
        .send((dispatch.id, "invalid target".to_string()))
        .await
        .unwrap();

    // A dead-lettered dispatch is never redelivered.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            channels.dispatches.recv(),
        )
        .await
        .is_err(),
        "failed dispatch must not be redelivered",
    );

    drop(channels.inbound);
    task.await.unwrap().unwrap();

    // Restart over the same storage: the failed dispatch is gone from the pending
    // set, so startup redelivery sends nothing.
    let (runtime, mut channels, _observer) = Runtime::channelled(core_over(storage), 8);
    let task = tokio::spawn(runtime.run());
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            channels.dispatches.recv(),
        )
        .await
        .is_err(),
        "restart must not redeliver a dead-lettered dispatch",
    );
    drop(channels.inbound);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn only_the_assigned_session_may_fail_a_dispatch() {
    let (runtime, runtime_channels, _observer) = Runtime::channelled(core(), 8);
    let (hub, handle) = ConnectorHub::new(runtime_channels, registrations());
    let runtime_task = tokio::spawn(runtime.run());
    let hub_task = tokio::spawn(hub.run());
    let mut whatsapp = handle.connect(registrations()[0].clone()).await.unwrap();
    let mut telegram = handle.connect(registrations()[1].clone()).await.unwrap();

    whatsapp
        .input
        .send(ConnectorInput::Event(wa_event("wa-1", None)))
        .await
        .unwrap();
    let ConnectorOutput::Dispatch(dispatch) = telegram.output.recv().await.unwrap() else {
        panic!("Telegram should receive the dispatch");
    };

    // A session that was not assigned the dispatch cannot fail it.
    whatsapp
        .input
        .send(ConnectorInput::DispatchFailed {
            dispatch_id: dispatch.id,
            reason: "not mine".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        whatsapp.output.recv().await.unwrap(),
        ConnectorOutput::Rejected { .. }
    ));

    // The assigned session dead-letters it: accepted without rejection, and a
    // fresh session after reconnect receives no redelivery.
    telegram
        .input
        .send(ConnectorInput::DispatchFailed {
            dispatch_id: dispatch.id,
            reason: "cannot deliver".into(),
        })
        .await
        .unwrap();
    // Flush the ordered input pipeline: a follow-up input on the same session is
    // processed after the DispatchFailed, so receiving its rejection proves the
    // dead-lettering (assignment removal) happened before we reconnect — no sleep
    // needed to observe it.
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
    drop(telegram);
    let mut telegram = handle.connect(registrations()[1].clone()).await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            telegram.output.recv()
        )
        .await
        .is_err(),
        "a dead-lettered dispatch is not redelivered after reconnect",
    );

    hub_task.abort();
    runtime_task.abort();
}

#[tokio::test]
async fn restart_with_orphaned_stash_replays_immediately() {
    let storage = Arc::new(InMemoryStorage::default());

    // Pre-crash state, built deterministically with a core: A provisions the
    // mirror thread and B is deferred into the durable stash.
    {
        let core = mirror_core(Arc::clone(&storage));
        let first = core.ingest(wa_group_event("wa-1")).await.unwrap();
        let provision = &first.dispatches()[0];
        assert!(provision.command.target.provision_thread.is_some());
        let provision_id = provision.id;
        let deferred = core.ingest(wa_group_event("wa-2")).await.unwrap();
        assert!(matches!(deferred, IngestOutcome::Deferred { .. }));

        // Simulate a crash after the provision settled but before its stash was
        // replayed: mark it failed in storage directly (not via the core, which
        // would replay right away), leaving B orphaned in the durable stash.
        storage
            .fail_dispatch(provision_id, "crashed mid-provision".into())
            .await
            .unwrap();
    }

    // Restart: the runtime's startup recovery drains the orphaned stash, so B's
    // dispatch appears without any acknowledgement, re-attempting provisioning.
    let (runtime, mut channels, _observer) = Runtime::channelled(mirror_core(storage), 8);
    let task = tokio::spawn(runtime.run());
    let replayed = channels.dispatches.recv().await.unwrap();
    assert!(
        replayed.command.target.provision_thread.is_some(),
        "the orphaned stash replays and re-attempts provisioning at startup",
    );
    drop(channels.inbound);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn connector_sessions_route_dispatches_and_authorize_inputs() {
    let (runtime, runtime_channels, _observer) = Runtime::channelled(core(), 8);
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
    let ConnectorOutput::Dispatch(parent_dispatch) = telegram.output.recv().await.unwrap() else {
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
    let ConnectorOutput::Dispatch(reply_dispatch) = telegram.output.recv().await.unwrap() else {
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
                    animated: false,
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
                    animated: false,
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
                    animated: false,
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
    let (runtime, runtime_channels, _observer) = Runtime::channelled(core(), 8);
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

async fn send_event(channels: &RuntimeChannels, event: NormalizedEvent) {
    channels
        .inbound
        .send(InboundWork::Event(event))
        .await
        .unwrap();
}

fn wa_event(native_id: &str, reply_to: Option<&str>) -> NormalizedEvent {
    source_event(
        ConnectorId(WA.into()),
        PlatformAddress::new("wa-chat", None),
        native_id,
        reply_to,
    )
}

fn tg_event(native_id: &str, reply_to: Option<&str>) -> NormalizedEvent {
    source_event(
        ConnectorId(TG.into()),
        PlatformAddress::new("tg-chat", Some("topic".into())),
        native_id,
        reply_to,
    )
}

fn source_event(
    connector_id: ConnectorId,
    address: PlatformAddress,
    native_id: &str,
    reply_to: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
        source: wuphf_model::SourceAddress {
            connector_id,
            address,
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

fn raw_outbound(chat_id: &str, text: &str) -> wuphf_model::RawOutbound {
    wuphf_model::RawOutbound {
        connector_id: ConnectorId(WA.into()),
        chat_id: chat_id.into(),
        thread_id: None,
        text: text.into(),
        reply_to_platform_message_id: None,
        timestamp_ms: 42,
    }
}

#[tokio::test]
async fn raw_send_travels_the_dispatch_loop_and_settles_an_outgoing_observation() {
    let (runtime, mut channels, observer) = Runtime::channelled(core(), 8);
    let sender = runtime.raw_sender();
    let task = tokio::spawn(runtime.run());
    let mut live = observer.subscribe();

    let settled = sender
        .send(raw_outbound("wa-chat", "from the tui"))
        .await
        .unwrap();

    // The direct WhatsApp delivery plus the routed mirror copy for telegram.
    let first = channels.dispatches.recv().await.unwrap();
    let second = channels.dispatches.recv().await.unwrap();
    let direct = [&first, &second]
        .into_iter()
        .find(|dispatch| dispatch.command.target_connector == ConnectorId(WA.into()))
        .expect("the raw send dispatches to its own connector");
    assert!(
        [&first, &second]
            .into_iter()
            .any(|dispatch| dispatch.command.target_connector == ConnectorId(TG.into())),
        "a raw send into a routed chat also feeds the mirror",
    );

    channels
        .acknowledgements
        .send(DispatchAcknowledgement {
            dispatch_id: direct.id,
            platform_message_id: "wa-raw-1".into(),
            thread_id: None,
        })
        .await
        .unwrap();

    // The acknowledgement settles the observation: waiter, live feed, and log
    // all carry the outgoing row stamped with the native id.
    let observed = settled.await.unwrap();
    assert_eq!(observed.platform_message_id, "wa-raw-1");
    assert_eq!(observed.direction, wuphf_model::ObservedDirection::Outgoing);
    let broadcast = live.recv().await.unwrap();
    assert_eq!(broadcast.platform_message_id, "wa-raw-1");
    let page = observer
        .storage()
        .observed_messages(&ConnectorId(WA.into()), "wa-chat", None, 10)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].direction, wuphf_model::ObservedDirection::Outgoing);

    drop(channels.inbound);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn observer_service_send_message_returns_the_acknowledged_native_id() {
    use crate::observer_service::GrpcObserverService;
    use wuphf_proto::connector::observer_transport_server::ObserverTransport;

    let (runtime, channels, observer) = Runtime::channelled(core(), 8);
    let service = GrpcObserverService::new(Arc::clone(&observer)).with_sender(runtime.raw_sender());
    let task = tokio::spawn(runtime.run());

    // Stand in for the WhatsApp connector: acknowledge its dispatch on arrival.
    let RuntimeChannels {
        inbound,
        acknowledgements,
        failures: _failures,
        mut dispatches,
    } = channels;
    let acker = tokio::spawn(async move {
        while let Some(dispatch) = dispatches.recv().await {
            if dispatch.command.target_connector == ConnectorId(WA.into()) {
                acknowledgements
                    .send(DispatchAcknowledgement {
                        dispatch_id: dispatch.id,
                        platform_message_id: "wa-raw-9".into(),
                        thread_id: None,
                    })
                    .await
                    .unwrap();
            }
        }
    });

    let response = service
        .send_message(tonic::Request::new(pb::SendMessageRequest {
            connector_id: WA.into(),
            chat_id: "wa-chat".into(),
            thread_id: None,
            text: "hello from the tui".into(),
            reply_to_platform_message_id: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.platform_message_id, "wa-raw-9");

    drop(inbound);
    task.await.unwrap().unwrap();
    acker.abort();
}
