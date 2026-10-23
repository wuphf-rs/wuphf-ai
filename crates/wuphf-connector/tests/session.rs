//! Session-loop tests against an in-process core served over gRPC, mirroring
//! the harness in `crates/wuphf-runtime/src/tests.rs`: a real Runtime,
//! ConnectorHub and GrpcConnectorService on a loopback listener, with the SDK
//! client as the connector under test.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use wuphf_connector::{Client, Delivery, Failure, Options};
use wuphf_core::{Core, RoutingConfig};
use wuphf_model::{
    ChatAddress, Connector, ConnectorCapabilities, ConnectorId, Event, EventKind, InboundWork,
    NewMessage, Platform, Rule, RuleId, RuleKind,
};
use wuphf_proto::connector as pb;
use wuphf_runtime::Runtime;
use wuphf_runtime::connector::ConnectorHub;
use wuphf_runtime::grpc::GrpcConnectorService;
use wuphf_storage::{DispatchStore, InMemoryStorage};

const WA: &str = "whatsapp-main";
const TG: &str = "telegram-main";

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
            capabilities: ConnectorCapabilities {
                supports_threads: true,
                supports_topic_create: true,
            },
        },
    ]
}

fn routing() -> RoutingConfig {
    let rules = vec![Rule {
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
    }];
    let connectors = vec![
        Connector {
            id: ConnectorId(WA.into()),
            platform: Platform::WhatsApp,
            capabilities: ConnectorCapabilities::default(),
        },
        Connector {
            id: ConnectorId(TG.into()),
            platform: Platform::Telegram,
            capabilities: ConnectorCapabilities {
                supports_threads: true,
                supports_topic_create: true,
            },
        },
    ];
    RoutingConfig::new(rules, connectors)
}

struct Harness {
    address: String,
    inbound: mpsc::Sender<InboundWork>,
    storage: Arc<InMemoryStorage>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Harness {
    async fn start() -> Self {
        let storage = Arc::new(InMemoryStorage::default());
        let core = Arc::new(Core::new(storage.clone(), Arc::new(routing())));
        let (runtime, channels, _observer) = Runtime::channelled(core, 8);
        let inbound = channels.inbound.clone();
        let (hub, handle) = ConnectorHub::new(channels, registrations());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let tasks = vec![
            tokio::spawn(async move {
                runtime.run().await.unwrap();
            }),
            tokio::spawn(async move { hub.run().await }),
            tokio::spawn(async move {
                Server::builder()
                    .add_service(
                        wuphf_proto::connector::connector_transport_server::ConnectorTransportServer::new(
                            GrpcConnectorService::new(handle),
                        ),
                    )
                    .serve_with_incoming(TcpListenerStream::new(listener))
                    .await
                    .unwrap();
            }),
        ];
        Self {
            address,
            inbound,
            storage,
            tasks,
        }
    }

    fn abort(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}

fn sdk_client(address: &str) -> Client {
    Client::new(Options {
        core_address: address.to_owned(),
        connector_id: TG.into(),
        platform: pb::Platform::Telegram,
        supports_threads: true,
        supports_topic_create: true,
        is_permanent: None,
    })
}

fn wa_event(native_id: &str, text: &str) -> Event {
    Event {
        source: wuphf_model::SourceAddress {
            connector_id: ConnectorId(WA.into()),
            address: ChatAddress::new("wa-chat", None),
            chat_title: None,
        },
        payload: EventKind::NewMessage(NewMessage {
            native_id: native_id.into(),
            author_id: "sender".into(),
            participant: None,
            author_name: None,
            text: Some(text.into()),
            attachments: vec![],
            reply_to_native_id: None,
            timestamp_ms: 1,
        }),
    }
}

/// Poll one dispatch until `predicate` matches its persisted state.
async fn wait_for_dispatch_state(
    storage: &InMemoryStorage,
    dispatch_id: &wuphf_model::DispatchId,
    predicate: impl Fn(&wuphf_model::Dispatch) -> bool,
) -> wuphf_model::Dispatch {
    for _ in 0..100 {
        if let Some(dispatch) = storage.dispatch(*dispatch_id).await.unwrap()
            && predicate(&dispatch)
        {
            return dispatch;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("dispatch did not reach the expected state in time");
}

#[derive(Clone)]
struct HarnessSender {
    received: Arc<StdMutex<Vec<pb::MessageCreateDispatch>>>,
    failure: Option<String>,
}

impl HarnessSender {
    fn succeeding(received: Arc<StdMutex<Vec<pb::MessageCreateDispatch>>>) -> Self {
        Self {
            received,
            failure: None,
        }
    }

    fn failing(received: Arc<StdMutex<Vec<pb::MessageCreateDispatch>>>, reason: &str) -> Self {
        Self {
            received,
            failure: Some(reason.into()),
        }
    }
}

impl wuphf_connector::Sender for HarnessSender {
    async fn send_message(&self, dispatch: pb::MessageCreateDispatch) -> Result<Delivery, Failure> {
        self.received.lock().unwrap().push(dispatch);
        match &self.failure {
            Some(reason) => Err(Failure::permanent(reason.clone())),
            None => Ok(Delivery {
                message_id: "tg-1".into(),
                thread_id: None,
            }),
        }
    }

    async fn edit_message(&self, _dispatch: pb::MessageEditDispatch) -> Result<String, Failure> {
        Ok("tg-1".into())
    }

    async fn delete_message(
        &self,
        _dispatch: pb::MessageDeleteDispatch,
    ) -> Result<String, Failure> {
        Ok("tg-1".into())
    }
}

#[tokio::test]
async fn registered_connector_receives_dispatch_and_acknowledges() {
    let harness = Harness::start().await;
    let received = Arc::new(StdMutex::new(Vec::new()));
    let (inputs_tx, inputs_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let session = tokio::spawn({
        let cancel = cancel.clone();
        let client = sdk_client(&harness.address);
        let sender = HarnessSender::succeeding(received.clone());
        async move { client.run(sender, inputs_rx, cancel).await }
    });

    harness
        .inbound
        .send(InboundWork::Event(wa_event("wa-1", "over the sdk")))
        .await
        .unwrap();

    // The dispatch reaches the sender with the rule's target and the event's text.
    let dispatch = wait_for("dispatch delivered to sender", || {
        received.lock().unwrap().first().cloned()
    })
    .await;
    assert_eq!(dispatch.target_chat_id, "tg-chat");
    assert_eq!(dispatch.target_thread_id.as_deref(), Some("topic"));
    assert_eq!(dispatch.text.as_deref(), Some("over the sdk"));

    // The acknowledgement settles the dispatch in storage.
    let dispatch_id = wuphf_model::DispatchId(dispatch.dispatch_id.parse().unwrap());
    wait_for_dispatch_state(&harness.storage, &dispatch_id, |dispatch| {
        matches!(
            dispatch.state,
            wuphf_model::DispatchState::Acknowledged { .. }
        )
    })
    .await;

    cancel.cancel();
    session.await.unwrap().unwrap();
    drop(inputs_tx);
    harness.abort();
}

#[tokio::test]
async fn permanent_failure_dead_letters_the_dispatch() {
    let harness = Harness::start().await;
    let received = Arc::new(StdMutex::new(Vec::new()));
    let (inputs_tx, inputs_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let session = tokio::spawn({
        let cancel = cancel.clone();
        let client = sdk_client(&harness.address);
        let sender = HarnessSender::failing(received.clone(), "bad target");
        async move { client.run(sender, inputs_rx, cancel).await }
    });

    harness
        .inbound
        .send(InboundWork::Event(wa_event("wa-2", "doomed")))
        .await
        .unwrap();

    let dispatch = wait_for("dispatch delivered to sender", || {
        received.lock().unwrap().first().cloned()
    })
    .await;
    let dispatch_id = wuphf_model::DispatchId(dispatch.dispatch_id.parse().unwrap());
    wait_for_dispatch_state(&harness.storage, &dispatch_id, |dispatch| {
        matches!(dispatch.state, wuphf_model::DispatchState::Failed { .. })
    })
    .await;

    // A dead-lettered dispatch is not redelivered: exactly one send attempt.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(received.lock().unwrap().len(), 1);

    cancel.cancel();
    session.await.unwrap().unwrap();
    drop(inputs_tx);
    harness.abort();
}

#[tokio::test]
async fn inbound_event_frames_are_forwarded_to_the_core() {
    let harness = Harness::start().await;
    let received = Arc::new(StdMutex::new(Vec::new()));
    let (inputs_tx, inputs_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let session = tokio::spawn({
        let cancel = cancel.clone();
        let client = sdk_client(&harness.address);
        let sender = HarnessSender::succeeding(received.clone());
        async move { client.run(sender, inputs_rx, cancel).await }
    });

    // A second SDK session registered as WhatsApp sends the event; the
    // Telegram session under test must receive the routed dispatch.
    let (wa_inputs_tx, wa_inputs_rx) = mpsc::channel(8);
    let wa_cancel = CancellationToken::new();
    let wa_session = tokio::spawn({
        let cancel = wa_cancel.clone();
        let client = Client::new(Options {
            core_address: harness.address.clone(),
            connector_id: WA.into(),
            platform: pb::Platform::Whatsapp,
            supports_threads: false,
            supports_topic_create: false,
            is_permanent: None,
        });
        let received = Arc::new(StdMutex::new(Vec::new()));
        let sender = HarnessSender::succeeding(received);
        async move { client.run(sender, wa_inputs_rx, cancel).await }
    });

    wa_inputs_tx
        .send(pb::ClientFrame {
            body: Some(pb::client_frame::Body::MessageCreate(
                pb::MessageCreateEvent {
                    chat_id: "wa-chat".into(),
                    thread_id: None,
                    chat_title: None,
                    platform_message_id: "wa-3".into(),
                    sender: "sender".into(),
                    participant: None,
                    text: Some("event through sdk".into()),
                    attachments: vec![],
                    reply_to_platform_message_id: None,
                    timestamp_ms: 1,
                    sender_name: None,
                },
            )),
        })
        .await
        .unwrap();

    let dispatch = wait_for("event routed to telegram", || {
        received.lock().unwrap().first().cloned()
    })
    .await;
    assert_eq!(dispatch.text.as_deref(), Some("event through sdk"));

    cancel.cancel();
    wa_cancel.cancel();
    session.await.unwrap().unwrap();
    wa_session.await.unwrap().unwrap();
    drop(inputs_tx);
    drop(wa_inputs_tx);
    harness.abort();
}

/// Poll `produce` until it yields Some or the deadline passes.
async fn wait_for<T>(what: &str, mut produce: impl FnMut() -> Option<T>) -> T {
    for _ in 0..100 {
        if let Some(value) = produce() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}
