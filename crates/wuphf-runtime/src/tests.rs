use super::*;
use crate::connector::ConnectorHub;
use crate::grpc::GrpcConnectorService;
use crate::media::GrpcMediaService;
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Server;
use wuphf_core::RoutingConfig;
use wuphf_model::{
    Action, ChatAddress, ChatFilter, Connector, ConnectorCapabilities, ConnectorId, ConnectorInput,
    ConnectorOutput, EventKind, IngestOutcome, NewMessage, NoticeKind, Platform, Rule, RuleId,
    RuleKind,
};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_client::ConnectorTransportClient;
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_proto::connector::media_transport_client::MediaTransportClient;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_storage::InMemoryStorage;
use wuphf_storage::{ChatProfileStore, DispatchStore, ObservationStore};

const WA: &str = "whatsapp-main";
const TG: &str = "telegram-main";

fn rules() -> Vec<Rule> {
    vec![Rule {
        id: RuleId("direct".into()),
        flow: RuleKind::OneToOne {
            source_connector: ConnectorId(WA.into()),
            source_chat: ChatAddress {
                chat_id: "wa-chat".into(),
                thread_id: None,
            },
            destination_connector: ConnectorId(TG.into()),
            destination_chat: ChatAddress {
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
        supports_disappearing_timers: false,
    }
}

fn registrations() -> Vec<Connector> {
    vec![
        Connector {
            id: ConnectorId(WA.into()),
            platform: Platform::WhatsApp,
            capabilities: ConnectorCapabilities::default(),
        },
        Connector {
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
    let config = Arc::new(RoutingConfig::new(rules(), registrations()));
    Arc::new(Core::new(storage, config))
}

/// A many-to-one mirror rule whose destination thread must be provisioned on
/// first send — the flow that stashes concurrent messages behind provisioning.
fn mirror_rules() -> Vec<Rule> {
    vec![Rule {
        id: RuleId("mirror".into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WA.into()),
            source_chats_filter: ChatFilter::Only {
                chats: vec![ChatAddress {
                    chat_id: "wa-group".into(),
                    thread_id: None,
                }],
            },
            destination_connector: ConnectorId(TG.into()),
            destination_chat: ChatAddress {
                chat_id: "tg-group".into(),
                thread_id: None,
            },
        },
    }]
}

fn mirror_core(storage: Arc<InMemoryStorage>) -> Arc<Core<InMemoryStorage>> {
    let config = Arc::new(RoutingConfig::new(mirror_rules(), registrations()));
    Arc::new(Core::new(storage, config))
}

fn wa_group_event(native_id: &str) -> Event {
    source_event(
        ConnectorId(WA.into()),
        ChatAddress::new("wa-group", None),
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
    assert_eq!(dispatch.to.connector_id, ConnectorId(TG.into()));
    assert_eq!(dispatch.to.address.chat_id, "tg-chat");
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
    let targets = [first.to.connector_id, second.to.connector_id];
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
            old: ChatAddress::new("wa-chat", None),
            new: ChatAddress::new("wa-chat-migrated", None),
        })
        .await
        .unwrap();
    send_event(
        &channels,
        source_event(
            ConnectorId(WA.into()),
            ChatAddress::new("wa-chat-migrated", None),
            "wa-2",
            None,
        ),
    )
    .await;

    // The message under the migrated address routes only if the migration was
    // applied first; otherwise it matches no rule and yields no dispatch.
    let dispatch = channels.dispatches.recv().await.unwrap();
    assert_eq!(dispatch.to.connector_id, ConnectorId(TG.into()));
    assert_eq!(dispatch.to.address.chat_id, "tg-chat");
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
        .send(DispatchAck {
            dispatch_id: parent_dispatch.id,
            native_id: "tg-parent".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    // Acknowledging the parent also emits a delivery-ack to the origin, so skip
    // to the released reply create among the dispatches.
    let reply_context = loop {
        let dispatch = channels.dispatches.recv().await.unwrap();
        if let Action::Send {
            reply: reply_context,
            ..
        } = dispatch.action
        {
            break reply_context.expect("reply context set on release");
        }
    };
    assert_eq!(reply_context.native_id, "tg-parent");
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
        .send(DispatchFailed {
            dispatch_id: dispatch.id,
            reason: "invalid target".to_string(),
        })
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
        .send(ConnectorInput::event(wa_event("wa-1", None)))
        .await
        .unwrap();
    let ConnectorOutput::Dispatch(dispatch) = telegram.output.recv().await.unwrap() else {
        panic!("Telegram should receive the dispatch");
    };

    // A session that was not assigned the dispatch cannot fail it.
    whatsapp
        .input
        .send(ConnectorInput::DispatchFailed(DispatchFailed {
            dispatch_id: dispatch.id,
            reason: "not mine".into(),
        }))
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
        .send(ConnectorInput::DispatchFailed(DispatchFailed {
            dispatch_id: dispatch.id,
            reason: "cannot deliver".into(),
        }))
        .await
        .unwrap();
    // Flush the ordered input pipeline: a follow-up input on the same session is
    // processed after the DispatchFailed, so receiving its rejection proves the
    // dead-lettering (assignment removal) happened before we reconnect — no sleep
    // needed to observe it.
    telegram
        .input
        .send(ConnectorInput::Acknowledgement(DispatchAck {
            dispatch_id: wuphf_model::DispatchId::new(),
            native_id: "ghost".into(),
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

/// Whatever connector id a frame carries, the hub stamps the session's own on
/// every work item (event, migration and notice), so a connector cannot speak
/// for another.
#[tokio::test]
async fn hub_stamps_the_session_connector_on_all_work() {
    let (inbound, mut inbound_rx) = mpsc::channel(8);
    let (acknowledgements, _acknowledgements) = mpsc::channel(8);
    let (failures, _failures) = mpsc::channel(8);
    let (_dispatch_tx, dispatches) = mpsc::channel(8);
    let channels = RuntimeChannels {
        inbound,
        acknowledgements,
        failures,
        dispatches,
    };
    let (hub, handle) = ConnectorHub::new(channels, registrations());
    let hub_task = tokio::spawn(hub.run());
    let whatsapp = handle.connect(registrations()[0].clone()).await.unwrap();

    let impostor = ConnectorId(TG.into());
    let mut event = wa_event("wa-1", None);
    event.source.connector_id = impostor.clone();
    let work = [
        InboundWork::Event(event),
        InboundWork::Migration {
            connector_id: impostor.clone(),
            old: ChatAddress::new("wa-chat", None),
            new: ChatAddress::new("wa-chat-migrated", None),
        },
        InboundWork::Notice {
            connector_id: impostor,
            kind: NoticeKind::Degraded,
            detail: "rate limited".into(),
        },
    ];
    for item in work {
        whatsapp
            .input
            .send(ConnectorInput::Work(Box::new(item)))
            .await
            .unwrap();
    }
    for _ in 0..3 {
        let item = inbound_rx.recv().await.unwrap();
        assert_eq!(item.connector_id(), &ConnectorId(WA.into()));
    }

    hub_task.abort();
}

#[tokio::test]
async fn restart_with_orphaned_stash_replays_immediately() {
    let storage = Arc::new(InMemoryStorage::default());

    // Pre-crash state, built deterministically with a core: the info card
    // provisions the mirror thread with A held in the durable stash behind it,
    // and B is deferred into the stash too.
    {
        let core = mirror_core(Arc::clone(&storage));
        let first = core.ingest(wa_group_event("wa-1")).await.unwrap();
        let provision = &first.dispatches()[0];
        assert!(provision.to.create_thread.is_some());
        let provision_id = provision.id;
        let deferred = core.ingest(wa_group_event("wa-2")).await.unwrap();
        assert!(matches!(deferred, IngestOutcome::Deferred { .. }));

        // Simulate a crash after the provision settled but before its stash was
        // replayed: mark it failed in storage directly (not via the core, which
        // would replay right away), leaving A and B orphaned in the stash.
        storage
            .fail_dispatch(provision_id, "crashed mid-provision".into())
            .await
            .unwrap();
    }

    // Restart: the runtime's startup recovery drains the orphaned stash, so A's
    // dispatch appears without any acknowledgement, re-attempting provisioning
    // (B waits behind it).
    let (runtime, mut channels, _observer) = Runtime::channelled(mirror_core(storage), 8);
    let task = tokio::spawn(runtime.run());
    let replayed = channels.dispatches.recv().await.unwrap();
    assert!(
        replayed.to.create_thread.is_some(),
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
            .connect(Connector {
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
        .send(ConnectorInput::Acknowledgement(DispatchAck {
            dispatch_id: wuphf_model::DispatchId::new(),
            native_id: "ghost".into(),
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
        .send(ConnectorInput::event(wa_event("wa-parent", None)))
        .await
        .unwrap();
    let ConnectorOutput::Dispatch(parent_dispatch) = telegram.output.recv().await.unwrap() else {
        panic!("Telegram should receive parent dispatch");
    };
    assert_eq!(parent_dispatch.to.address.chat_id, "tg-chat");
    drop(telegram);
    let mut telegram = handle.connect(registrations()[1].clone()).await.unwrap();
    let ConnectorOutput::Dispatch(redelivered_parent) = telegram.output.recv().await.unwrap()
    else {
        panic!("Telegram should receive unacknowledged dispatch after reconnect");
    };
    assert_eq!(redelivered_parent.id, parent_dispatch.id);
    whatsapp
        .input
        .send(ConnectorInput::event(wa_event(
            "wa-reply",
            Some("wa-parent"),
        )))
        .await
        .unwrap();
    telegram
        .input
        .send(ConnectorInput::Acknowledgement(DispatchAck {
            dispatch_id: parent_dispatch.id,
            native_id: "tg-parent".into(),
            thread_id: None,
        }))
        .await
        .unwrap();
    let ConnectorOutput::Dispatch(reply_dispatch) = telegram.output.recv().await.unwrap() else {
        panic!("Telegram should receive released reply dispatch");
    };
    let reply_dispatch = *reply_dispatch;
    let Action::Send {
        reply: reply_context,
        ..
    } = reply_dispatch.action
    else {
        panic!("released reply should be a message create");
    };
    let reply_context = reply_context.expect("reply context set on release");
    assert_eq!(reply_context.native_id, "tg-parent");

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
                connector_tokens(),
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
                supports_disappearing_timers: false,
            }),
        ))
        .await
        .unwrap();
    let mut telegram_output = ConnectorTransportClient::connect(uri.clone())
        .await
        .unwrap()
        .open_session(with_token(ReceiverStream::new(telegram_rx), TG_TOKEN))
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
        .open_session(with_token(ReceiverStream::new(whatsapp_rx), WA_TOKEN))
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
                    disappearing: None,
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
            settings: None,
        })),
    }
}

async fn send_event(channels: &RuntimeChannels, event: Event) {
    channels
        .inbound
        .send(InboundWork::Event(event))
        .await
        .unwrap();
}

fn wa_event(native_id: &str, reply_to: Option<&str>) -> Event {
    source_event(
        ConnectorId(WA.into()),
        ChatAddress::new("wa-chat", None),
        native_id,
        reply_to,
    )
}

fn tg_event(native_id: &str, reply_to: Option<&str>) -> Event {
    source_event(
        ConnectorId(TG.into()),
        ChatAddress::new("tg-chat", Some("topic".into())),
        native_id,
        reply_to,
    )
}

fn source_event(
    connector_id: ConnectorId,
    address: ChatAddress,
    native_id: &str,
    reply_to: Option<&str>,
) -> Event {
    Event {
        source: wuphf_model::SourceAddress {
            connector_id,
            address,
            chat_title: None,
        },
        payload: EventKind::NewMessage(NewMessage {
            native_id: native_id.into(),
            author_id: "sender".into(),
            participant: None,
            author_name: None,
            text: Some("message".into()),
            attachments: vec![],
            reply_to_native_id: reply_to.map(str::to_owned),
            timestamp_ms: 1,
            disappearing: None,
        }),
    }
}

fn owner_message(chat_id: &str, text: &str) -> wuphf_model::OwnerMessage {
    wuphf_model::OwnerMessage {
        connector_id: ConnectorId(WA.into()),
        chat_id: chat_id.into(),
        thread_id: None,
        text: text.into(),
        media_ids: Vec::new(),
        client: None,
        reply_to_native_id: None,
        timestamp_ms: 42,
    }
}

#[tokio::test]
async fn owner_message_travels_the_dispatch_loop_and_settles_an_outgoing_observation() {
    let (runtime, mut channels, observer) = Runtime::channelled(core(), 8);
    let sender = runtime.owner_sender();
    let task = tokio::spawn(runtime.run());
    let mut live = observer.subscribe();

    let settled = sender
        .send(owner_message("wa-chat", "from the tui"))
        .await
        .unwrap();

    // The direct WhatsApp delivery plus the routed mirror copy for telegram.
    let first = channels.dispatches.recv().await.unwrap();
    let second = channels.dispatches.recv().await.unwrap();
    let direct = [&first, &second]
        .into_iter()
        .find(|dispatch| dispatch.to.connector_id == ConnectorId(WA.into()))
        .expect("the owner message dispatches to its own connector");
    assert!(
        [&first, &second]
            .into_iter()
            .any(|dispatch| dispatch.to.connector_id == ConnectorId(TG.into())),
        "an owner message into a routed chat also feeds the mirror",
    );

    channels
        .acknowledgements
        .send(DispatchAck {
            dispatch_id: direct.id,
            native_id: "wa-raw-1".into(),
            thread_id: None,
        })
        .await
        .unwrap();

    // The acknowledgement settles the observation: waiter, live feed, and log
    // all carry the outgoing row stamped with the native id.
    let observed = settled.await.unwrap();
    assert_eq!(observed.native_id, "wa-raw-1");
    assert_eq!(observed.direction, wuphf_model::ObservedDirection::Outgoing);
    let broadcast = live.recv().await.unwrap();
    assert_eq!(broadcast.native_id, "wa-raw-1");
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
    let service =
        GrpcObserverService::new(Arc::clone(&observer)).with_sender(runtime.owner_sender());
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
            if dispatch.to.connector_id == ConnectorId(WA.into()) {
                acknowledgements
                    .send(DispatchAck {
                        dispatch_id: dispatch.id,
                        native_id: "wa-raw-9".into(),
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
            media_ids: Vec::new(),
            client: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.platform_message_id, "wa-raw-9");

    drop(inbound);
    task.await.unwrap().unwrap();
    acker.abort();
}

/// A client's media-only send: the core resolves the media ids, the outgoing
/// row carries the attachments and the client as `sent_by`, and the
/// platform's echo of the send keeps both.
#[tokio::test]
async fn observer_service_sends_media_ids_and_tags_the_client() {
    use crate::observer_service::GrpcObserverService;
    use tokio_stream::StreamExt;
    use wuphf_model::{Attachment, AttachmentKind, MediaBlob, MediaId, ObservedDirection};
    use wuphf_proto::connector::observer_transport_server::ObserverTransport;
    use wuphf_storage::MediaStore;

    let (runtime, channels, observer) = Runtime::channelled(core(), 8);
    let service =
        GrpcObserverService::new(Arc::clone(&observer)).with_sender(runtime.owner_sender());
    let task = tokio::spawn(runtime.run());
    let RuntimeChannels {
        inbound,
        acknowledgements,
        failures: _failures,
        mut dispatches,
    } = channels;
    let (sent_tx, mut sent_rx) = mpsc::channel(4);
    let acker = tokio::spawn(async move {
        while let Some(dispatch) = dispatches.recv().await {
            if dispatch.to.connector_id == ConnectorId(WA.into()) {
                sent_tx.send(dispatch.clone()).await.unwrap();
                acknowledgements
                    .send(DispatchAck {
                        dispatch_id: dispatch.id,
                        native_id: "wa-reel".into(),
                        thread_id: None,
                    })
                    .await
                    .unwrap();
            }
        }
    });

    let attachment = Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::Video,
        file_name: None,
        mime_type: Some("video/mp4".into()),
        size_bytes: Some(4),
        animated: false,
    };
    observer
        .storage()
        .put_media(
            MediaBlob {
                attachment: attachment.clone(),
                bytes: b"reel".to_vec(),
            },
            1,
        )
        .await
        .unwrap();
    let mut stream = service
        .subscribe_messages(tonic::Request::new(pb::SubscribeMessagesRequest::default()))
        .await
        .unwrap()
        .into_inner();

    let request = |text: &str, media_ids: Vec<String>| {
        tonic::Request::new(pb::SendMessageRequest {
            connector_id: WA.into(),
            chat_id: "wa-chat".into(),
            thread_id: None,
            text: text.into(),
            reply_to_platform_message_id: None,
            media_ids,
            client: Some("wuphf-misc".into()),
        })
    };
    // Neither text nor media, a malformed id, an id of no stored media.
    for (text, ids) in [
        ("  ", vec![]),
        ("", vec!["not-a-uuid".to_owned()]),
        ("caption", vec![MediaId::new().to_string()]),
    ] {
        let status = service.send_message(request(text, ids)).await.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status:?}");
    }

    let response = service
        .send_message(request("", vec![attachment.media_id.to_string()]))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.platform_message_id, "wa-reel");
    let direct = sent_rx.recv().await.unwrap();
    let Action::Send { message, .. } = &direct.action else {
        panic!("an owner send dispatches a send, got {:?}", direct.action);
    };
    assert_eq!(message.attachments, std::slice::from_ref(&attachment));

    let row = stream.next().await.unwrap().unwrap();
    assert_eq!(row.platform_message_id, "wa-reel");
    assert_eq!(row.sent_by.as_deref(), Some("wuphf-misc"));
    assert_eq!(row.text, None);
    assert_eq!(row.attachments.len(), 1);

    // WhatsApp reports the send back: the row stays outgoing and keeps its
    // client, so the client still recognises its own message.
    observer
        .observe(
            &Event {
                source: wuphf_model::SourceAddress {
                    connector_id: ConnectorId(WA.into()),
                    address: ChatAddress::new("wa-chat", None),
                    chat_title: None,
                },
                payload: EventKind::NewMessage(NewMessage {
                    native_id: "wa-reel".into(),
                    author_id: "me".into(),
                    participant: None,
                    author_name: None,
                    text: None,
                    attachments: vec![],
                    reply_to_native_id: None,
                    timestamp_ms: 50,
                    disappearing: None,
                }),
            },
            Platform::WhatsApp,
        )
        .await
        .unwrap();
    let echo = stream.next().await.unwrap().unwrap();
    assert_eq!(echo.direction, i32::from(pb::ObservedDirection::Outgoing));
    assert_eq!(echo.sent_by.as_deref(), Some("wuphf-misc"));
    let stored = observer
        .storage()
        .observed_messages(&ConnectorId(WA.into()), "wa-chat", None, 10)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].direction, ObservedDirection::Outgoing);
    assert_eq!(stored[0].sent_by.as_deref(), Some("wuphf-misc"));

    drop(inbound);
    task.await.unwrap().unwrap();
    acker.abort();
}

/// Hub channels the test drives directly: `inbound` receives what the hub
/// forwards to the runtime, `dispatches` feeds dispatches to the hub.
struct HubHarness {
    handle: crate::connector::ConnectorHubHandle,
    inbound: mpsc::Receiver<InboundWork>,
    dispatches: mpsc::Sender<Dispatch>,
    _acknowledgements: mpsc::Receiver<DispatchAck>,
    _failures: mpsc::Receiver<DispatchFailed>,
    task: tokio::task::JoinHandle<()>,
}

fn hub_harness() -> HubHarness {
    let (inbound_tx, inbound) = mpsc::channel(8);
    let (acknowledgements_tx, acknowledgements) = mpsc::channel(8);
    let (failures_tx, failures) = mpsc::channel(8);
    let (dispatches, dispatches_rx) = mpsc::channel(8);
    let (hub, handle) = ConnectorHub::new(
        RuntimeChannels {
            inbound: inbound_tx,
            acknowledgements: acknowledgements_tx,
            failures: failures_tx,
            dispatches: dispatches_rx,
        },
        registrations(),
    );
    HubHarness {
        handle,
        inbound,
        dispatches,
        _acknowledgements: acknowledgements,
        _failures: failures,
        task: tokio::spawn(hub.run()),
    }
}

fn telegram_dispatch() -> Dispatch {
    Dispatch::new(
        wuphf_model::Target {
            endpoint_id: wuphf_model::EndpointId::new(),
            connector_id: ConnectorId(TG.into()),
            address: ChatAddress::new("tg-chat", None),
            create_thread: None,
        },
        wuphf_model::ConversationId::new(),
        Action::Delete {
            message_id: wuphf_model::MessageId::new(),
            native_id: "tg-1".into(),
        },
        None,
    )
}

fn native_id_of(work: &InboundWork) -> &str {
    let InboundWork::Event(Event {
        payload: EventKind::NewMessage(message),
        ..
    }) = work
    else {
        panic!("expected a new message event, got {work:?}");
    };
    &message.native_id
}

#[tokio::test]
async fn second_live_session_is_rejected() {
    let harness = hub_harness();
    let mut first = harness
        .handle
        .connect(registrations()[1].clone())
        .await
        .unwrap();
    let second = harness.handle.connect(registrations()[1].clone()).await;
    assert_eq!(
        second.err(),
        Some(crate::connector::RegistrationError::AlreadyLive)
    );

    // The first session is untouched and still receives dispatches.
    let dispatch = telegram_dispatch();
    harness.dispatches.send(dispatch.clone()).await.unwrap();
    let ConnectorOutput::Dispatch(received) = first.output.recv().await.unwrap() else {
        panic!("the live session should receive the dispatch");
    };
    assert_eq!(received.id, dispatch.id);
    harness.task.abort();
}

#[tokio::test]
async fn reconnect_after_drop_is_accepted() {
    let harness = hub_harness();
    let mut first = harness
        .handle
        .connect(registrations()[1].clone())
        .await
        .unwrap();
    let dispatch = telegram_dispatch();
    harness.dispatches.send(dispatch.clone()).await.unwrap();
    assert!(matches!(
        first.output.recv().await.unwrap(),
        ConnectorOutput::Dispatch(_)
    ));
    drop(first);

    // The unacknowledged dispatch is redelivered to the new session.
    let mut second = harness
        .handle
        .connect(registrations()[1].clone())
        .await
        .expect("a session whose transport is gone no longer counts as live");
    let ConnectorOutput::Dispatch(redelivered) = second.output.recv().await.unwrap() else {
        panic!("the new session should receive the unacknowledged dispatch");
    };
    assert_eq!(redelivered.id, dispatch.id);
    harness.task.abort();
}

#[tokio::test]
async fn input_from_a_replaced_session_is_ignored() {
    let mut harness = hub_harness();
    let first = harness
        .handle
        .connect(registrations()[1].clone())
        .await
        .unwrap();
    // Keep the old session's input sender past the end of its transport, as a
    // forwarder still draining when the connector reconnects would.
    let stale_input = first.input.clone();
    drop(first);
    let second = harness
        .handle
        .connect(registrations()[1].clone())
        .await
        .unwrap();

    stale_input
        .send(ConnectorInput::event(tg_event("stale", None)))
        .await
        .unwrap();
    stale_input
        .send(ConnectorInput::event(tg_event("stale-barrier", None)))
        .await
        .unwrap();
    // Once the old forwarder has taken the barrier, "stale" is already queued at
    // the hub, ahead of anything the new session sends next.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while stale_input.capacity() < stale_input.max_capacity() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the old forwarder drains its queue");
    second
        .input
        .send(ConnectorInput::event(tg_event("fresh", None)))
        .await
        .unwrap();

    let forwarded = harness.inbound.recv().await.unwrap();
    assert_eq!(native_id_of(&forwarded), "fresh");
    harness.task.abort();
}

/// Serve the connector transport for `handle` on a local port; returns its URI.
async fn serve_connectors(
    handle: crate::connector::ConnectorHubHandle,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(ConnectorTransportServer::new(GrpcConnectorService::new(
                handle,
                connector_tokens(),
            )))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (format!("http://{address}"), task)
}

/// Open a Telegram session over gRPC; keeps the request sender alive in the
/// returned tuple so the session stays open until it is dropped.
async fn open_telegram_session(
    client: &mut ConnectorTransportClient<tonic::transport::Channel>,
    token: Option<&str>,
) -> Result<
    (
        mpsc::Sender<pb::ClientFrame>,
        tonic::Streaming<pb::ServerFrame>,
    ),
    tonic::Status,
> {
    let (frames, frames_rx) = mpsc::channel(8);
    frames
        .send(registration_frame(
            TG,
            pb::Platform::Telegram,
            Some(pb::Capabilities {
                supports_threads: true,
                supports_topic_create: true,
                supports_disappearing_timers: false,
            }),
        ))
        .await
        .unwrap();
    let mut request = tonic::Request::new(ReceiverStream::new(frames_rx));
    if let Some(token) = token {
        request = with_token(request.into_inner(), token);
    }
    let output = client.open_session(request).await?.into_inner();
    Ok((frames, output))
}

#[tokio::test]
async fn grpc_second_session_is_already_exists() {
    let harness = hub_harness();
    let (uri, server) = serve_connectors(harness.handle.clone()).await;
    let mut client = ConnectorTransportClient::connect(uri).await.unwrap();
    let _first = open_telegram_session(&mut client, Some(TG_TOKEN))
        .await
        .unwrap();
    let error = open_telegram_session(&mut client, Some(TG_TOKEN))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::AlreadyExists);
    server.abort();
    harness.task.abort();
}

#[tokio::test]
async fn grpc_session_after_client_disconnect_is_accepted() {
    let harness = hub_harness();
    let (uri, server) = serve_connectors(harness.handle.clone()).await;
    let mut first_client = ConnectorTransportClient::connect(uri.clone())
        .await
        .unwrap();
    let first = open_telegram_session(&mut first_client, Some(TG_TOKEN))
        .await
        .unwrap();
    drop(first);
    drop(first_client);

    // The server notices the closed stream on its own (a socket close, not a
    // timer); until it has, a new session is still refused.
    let mut client = ConnectorTransportClient::connect(uri).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match open_telegram_session(&mut client, Some(TG_TOKEN)).await {
                Ok(session) => return session,
                Err(error) if error.code() == tonic::Code::AlreadyExists => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
    })
    .await
    .expect("the new session registers once the old stream has ended");
    server.abort();
    harness.task.abort();
}

const WA_TOKEN: &str = "wa-token";
const TG_TOKEN: &str = "tg-token";
const PROVIDER_TOKEN: &str = "provider-token";

fn connector_tokens() -> Arc<crate::auth::ConnectorTokens> {
    Arc::new(crate::auth::ConnectorTokens::new(HashMap::from([
        (ConnectorId(WA.into()), WA_TOKEN.to_owned()),
        (ConnectorId(TG.into()), TG_TOKEN.to_owned()),
    ])))
}

/// A request carrying `authorization: Bearer <token>`.
fn with_token<T>(message: T, token: &str) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    request
}

#[tokio::test]
async fn grpc_session_without_token_is_unauthenticated() {
    let harness = hub_harness();
    let (uri, server) = serve_connectors(harness.handle.clone()).await;
    let mut client = ConnectorTransportClient::connect(uri).await.unwrap();
    let error = open_telegram_session(&mut client, None).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    server.abort();
    harness.task.abort();
}

#[tokio::test]
async fn grpc_session_with_another_connectors_token_is_unauthenticated() {
    let harness = hub_harness();
    let (uri, server) = serve_connectors(harness.handle.clone()).await;
    let mut client = ConnectorTransportClient::connect(uri).await.unwrap();
    let error = open_telegram_session(&mut client, Some(WA_TOKEN))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    // The refused attempt never reached the hub: the real connector still gets in.
    open_telegram_session(&mut client, Some(TG_TOKEN))
        .await
        .expect("the connector's own token registers");
    server.abort();
    harness.task.abort();
}

#[tokio::test]
async fn grpc_session_with_its_token_registers() {
    let harness = hub_harness();
    let (uri, server) = serve_connectors(harness.handle.clone()).await;
    let mut client = ConnectorTransportClient::connect(uri).await.unwrap();
    let (_frames, mut output) = open_telegram_session(&mut client, Some(TG_TOKEN))
        .await
        .unwrap();
    let dispatch = telegram_dispatch();
    harness.dispatches.send(dispatch.clone()).await.unwrap();
    let frame = output.message().await.unwrap().unwrap();
    let Some(pb::server_frame::Body::MessageDelete(delete)) = frame.body else {
        panic!("the session should receive the dispatch");
    };
    assert_eq!(delete.dispatch_id, dispatch.id.to_string());
    server.abort();
    harness.task.abort();
}

fn upload_frames() -> Vec<pb::UploadMediaFrame> {
    vec![
        pb::UploadMediaFrame {
            body: Some(pb::upload_media_frame::Body::Metadata(pb::MediaMetadata {
                kind: pb::attachment::Kind::File.into(),
                file_name: None,
                mime_type: None,
                size_bytes: Some(2),
                animated: false,
            })),
        },
        pb::UploadMediaFrame {
            body: Some(pb::upload_media_frame::Body::Chunk(b"hi".to_vec())),
        },
    ]
}

#[tokio::test]
async fn media_requires_a_known_token() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MediaTransportServer::with_interceptor(
                GrpcMediaService::new(
                    Arc::new(InMemoryStorage::default()),
                    1024,
                    std::time::Duration::from_secs(3600),
                ),
                crate::auth::any_token_auth(vec![TG_TOKEN.into(), PROVIDER_TOKEN.into()]),
            ))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let mut client = MediaTransportClient::connect(format!("http://{address}"))
        .await
        .unwrap();

    let error = client
        .upload_media(tokio_stream::iter(upload_frames()))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    let error = client
        .upload_media(with_token(tokio_stream::iter(upload_frames()), "unknown"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);

    let by_connector = client
        .upload_media(with_token(tokio_stream::iter(upload_frames()), TG_TOKEN))
        .await
        .expect("a connector token may upload")
        .into_inner();
    client
        .upload_media(with_token(
            tokio_stream::iter(upload_frames()),
            PROVIDER_TOKEN,
        ))
        .await
        .expect("a provider token may upload");

    let download = || pb::DownloadMediaRequest {
        media_id: by_connector.media_id.clone(),
    };
    let error = client.download_media(download()).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    let mut stream = client
        .download_media(with_token(download(), PROVIDER_TOKEN))
        .await
        .expect("a provider token may download")
        .into_inner();
    assert_eq!(
        stream.message().await.unwrap().unwrap().chunk,
        b"hi".to_vec()
    );
    client
        .download_media(with_token(download(), TG_TOKEN))
        .await
        .expect("a connector token may download");
    server.abort();
}

/// End to end over gRPC: a WhatsApp connector that declares disappearing
/// timers reports a chat's timer, and a Telegram reply into the mirrored chat
/// reaches WhatsApp stamped with it.
#[tokio::test]
async fn a_reported_timer_reaches_the_whatsapp_send_over_grpc() {
    let mut connectors = registrations();
    connectors[0].capabilities.supports_disappearing_timers = true;
    let storage = Arc::new(InMemoryStorage::default());
    let core = Arc::new(Core::new(
        Arc::clone(&storage),
        Arc::new(RoutingConfig::new(rules(), connectors.clone())),
    ));
    let (runtime, runtime_channels, _observer) = Runtime::channelled(core, 8);
    let (hub, handle) = ConnectorHub::new(runtime_channels, connectors);
    let runtime_task = tokio::spawn(runtime.run());
    let hub_task = tokio::spawn(hub.run());
    let (uri, server_task) = serve_connectors(handle).await;

    let (whatsapp_tx, whatsapp_rx) = mpsc::channel(8);
    whatsapp_tx
        .send(registration_frame(
            WA,
            pb::Platform::Whatsapp,
            Some(pb::Capabilities {
                supports_threads: false,
                supports_topic_create: false,
                supports_disappearing_timers: true,
            }),
        ))
        .await
        .unwrap();
    let mut whatsapp_output = ConnectorTransportClient::connect(uri.clone())
        .await
        .unwrap()
        .open_session(with_token(ReceiverStream::new(whatsapp_rx), WA_TOKEN))
        .await
        .unwrap()
        .into_inner();
    let (telegram_tx, telegram_rx) = mpsc::channel(8);
    telegram_tx
        .send(registration_frame(
            TG,
            pb::Platform::Telegram,
            Some(pb::Capabilities {
                supports_threads: true,
                supports_topic_create: true,
                supports_disappearing_timers: false,
            }),
        ))
        .await
        .unwrap();
    let _telegram_output = ConnectorTransportClient::connect(uri)
        .await
        .unwrap()
        .open_session(with_token(ReceiverStream::new(telegram_rx), TG_TOKEN))
        .await
        .unwrap()
        .into_inner();

    whatsapp_tx
        .send(pb::ClientFrame {
            body: Some(pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
                updates: vec![pb::ChatUpdate {
                    chat_id: "wa-chat".into(),
                    disappearing: Some(pb::DisappearingTimer {
                        seconds: 604_800,
                        setting_timestamp: Some(1_700_000_000),
                    }),
                    ..Default::default()
                }],
                snapshot: false,
            })),
        })
        .await
        .unwrap();
    // Each connector has its own ingest worker, so wait for the update to land
    // before the reply is routed on the Telegram worker.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while storage
            .chat_profile(&ConnectorId(WA.into()), "wa-chat")
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the chat update is applied");

    telegram_tx
        .send(pb::ClientFrame {
            body: Some(pb::client_frame::Body::MessageCreate(
                pb::MessageCreateEvent {
                    chat_id: "tg-chat".into(),
                    thread_id: Some("topic".into()),
                    chat_title: None,
                    platform_message_id: "tg-reply".into(),
                    sender: "owner".into(),
                    participant: None,
                    text: Some("reply".into()),
                    attachments: vec![],
                    reply_to_platform_message_id: None,
                    timestamp_ms: 1,
                    sender_name: None,
                    disappearing: None,
                },
            )),
        })
        .await
        .unwrap();
    let frame = whatsapp_output.message().await.unwrap().unwrap();
    let Some(pb::server_frame::Body::MessageCreate(dispatch)) = frame.body else {
        panic!("WhatsApp should receive the reply as a message dispatch");
    };
    assert_eq!(dispatch.target_chat_id, "wa-chat");
    assert_eq!(
        dispatch.disappearing,
        Some(pb::DisappearingTimer {
            seconds: 604_800,
            setting_timestamp: Some(1_700_000_000),
        })
    );

    server_task.abort();
    hub_task.abort();
    runtime_task.abort();
}
