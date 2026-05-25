use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Server;
use wuphf_core::{Config, ConnectorService, Engine, Store};
use wuphf_proto::v1::{
    connector_client::ConnectorClient, connector_server::ConnectorServer, connector_to_core,
    core_to_connector, ConnectorToCore, CreateMessage, DispatchFailed, DispatchSucceeded, Register,
};

struct Client {
    input: mpsc::Sender<ConnectorToCore>,
    output: tonic::Streaming<wuphf_proto::v1::CoreToConnector>,
}

struct TestServer {
    url: String,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl TestServer {
    async fn stop(self) {
        self.shutdown.send(()).unwrap();
        self.task.await.unwrap();
    }
}

async fn setup() -> (TestServer, TempDir, Arc<Config>) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("grpc.sqlite");
    let config: Config = toml::from_str(&format!(
        r#"
            [server]
            listen = "127.0.0.1:0"
            [database]
            url = "sqlite://{}"
            [delivery]
            acknowledgement_timeout_ms = 1000
            retry_initial_ms = 20
            retry_max_ms = 100
            [[connectors]]
            id = "a"
            token = "a-token"
            [[connectors]]
            id = "b"
            token = "b-token"
            [[endpoints]]
            id = "a-room"
            connector_id = "a"
            address = "a"
            [[endpoints]]
            id = "b-room"
            connector_id = "b"
            address = "b"
            [[replication_groups]]
            id = "rooms"
            endpoints = ["a-room", "b-room"]
        "#,
        path.display()
    ))
    .unwrap();
    config.validate().unwrap();
    let config = Arc::new(config);
    let server = spawn_server(config.clone()).await;
    (server, temp, config)
}

async fn spawn_server(config: Arc<Config>) -> TestServer {
    let store = Store::open(&config).await.unwrap();
    let service = ConnectorService::new(Engine::new(config, store));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(ConnectorServer::new(service))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                receiver.await.ok();
            })
            .await
            .unwrap();
    });
    TestServer {
        url: format!("http://{address}"),
        shutdown,
        task,
    }
}

async fn connect(server: &str, id: &str, token: &str) -> Client {
    let mut client = ConnectorClient::connect(server.to_string()).await.unwrap();
    let (input, messages) = mpsc::channel(8);
    input
        .send(ConnectorToCore {
            operation: Some(connector_to_core::Operation::Register(Register {
                connector_id: id.into(),
                shared_token: token.into(),
            })),
        })
        .await
        .unwrap();
    let output = client
        .connector_stream(ReceiverStream::new(messages))
        .await
        .unwrap()
        .into_inner();
    Client { input, output }
}

async fn registered(client: &mut Client, accepted: bool) {
    let response = client.output.message().await.unwrap().unwrap();
    let core_to_connector::Operation::RegistrationResult(result) = response.operation.unwrap()
    else {
        panic!("expected registration result");
    };
    assert_eq!(result.accepted, accepted);
}

fn create(event: &str, platform_id: &str, reply_to: Option<&str>) -> ConnectorToCore {
    ConnectorToCore {
        operation: Some(connector_to_core::Operation::CreateMessage(CreateMessage {
            event_id: event.into(),
            endpoint_id: "a-room".into(),
            platform_message_id: platform_id.into(),
            sender_id: "test".into(),
            sender_display_name: "Test".into(),
            sent_at_unix_ms: 10,
            text: event.into(),
            reply_to_platform_message_id: reply_to.map(str::to_owned),
            attachments: Vec::new(),
        })),
    }
}

#[tokio::test]
async fn streams_create_reply_and_replays_unacknowledged_dispatch() {
    let (server, _temp, _config) = setup().await;
    let mut a = connect(&server.url, "a", "a-token").await;
    let mut b = connect(&server.url, "b", "b-token").await;
    registered(&mut a, true).await;
    registered(&mut b, true).await;

    a.input
        .send(create("parent", "a-parent", None))
        .await
        .unwrap();
    let delivered = tokio::time::timeout(Duration::from_secs(1), b.output.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let core_to_connector::Operation::DispatchCreateMessage(parent) = delivered.operation.unwrap()
    else {
        panic!("expected dispatch");
    };

    let mut reconnected = connect(&server.url, "b", "b-token").await;
    registered(&mut reconnected, true).await;
    let replaced_status = b.output.message().await.unwrap_err();
    assert_eq!(replaced_status.code(), tonic::Code::Aborted);
    let replay = reconnected.output.message().await.unwrap().unwrap();
    let core_to_connector::Operation::DispatchCreateMessage(replay) = replay.operation.unwrap()
    else {
        panic!("expected replayed dispatch");
    };
    assert_eq!(replay.dispatch_id, parent.dispatch_id);
    reconnected
        .input
        .send(ConnectorToCore {
            operation: Some(connector_to_core::Operation::DispatchSucceeded(
                DispatchSucceeded {
                    dispatch_id: replay.dispatch_id,
                    platform_message_id: "b-parent".into(),
                },
            )),
        })
        .await
        .unwrap();

    a.input
        .send(create("reply", "a-reply", Some("a-parent")))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(1), reconnected.output.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let core_to_connector::Operation::DispatchCreateMessage(reply) = reply.operation.unwrap()
    else {
        panic!("expected reply dispatch");
    };
    assert_eq!(
        reply.reply_to_platform_message_id.as_deref(),
        Some("b-parent")
    );
}

#[tokio::test]
async fn rejects_invalid_connector_token() {
    let (server, _temp, _config) = setup().await;
    let mut client = connect(&server.url, "a", "wrong").await;
    registered(&mut client, false).await;
}

#[tokio::test]
async fn retries_transient_failure_and_does_not_retry_permanent_failure() {
    let (server, _temp, _config) = setup().await;
    let mut a = connect(&server.url, "a", "a-token").await;
    let mut b = connect(&server.url, "b", "b-token").await;
    registered(&mut a, true).await;
    registered(&mut b, true).await;

    a.input
        .send(create("transient", "a-transient", None))
        .await
        .unwrap();
    let first = b.output.message().await.unwrap().unwrap();
    let core_to_connector::Operation::DispatchCreateMessage(first) = first.operation.unwrap()
    else {
        panic!("expected transient dispatch");
    };
    b.input
        .send(ConnectorToCore {
            operation: Some(connector_to_core::Operation::DispatchFailed(
                DispatchFailed {
                    dispatch_id: first.dispatch_id.clone(),
                    reason: "try again".into(),
                    permanent: false,
                },
            )),
        })
        .await
        .unwrap();
    let retried = tokio::time::timeout(Duration::from_secs(1), b.output.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let core_to_connector::Operation::DispatchCreateMessage(retried) = retried.operation.unwrap()
    else {
        panic!("expected retried dispatch");
    };
    assert_eq!(retried.dispatch_id, first.dispatch_id);
    b.input
        .send(ConnectorToCore {
            operation: Some(connector_to_core::Operation::DispatchFailed(
                DispatchFailed {
                    dispatch_id: retried.dispatch_id,
                    reason: "invalid destination".into(),
                    permanent: true,
                },
            )),
        })
        .await
        .unwrap();

    assert!(
        tokio::time::timeout(Duration::from_millis(150), b.output.message())
            .await
            .is_err()
    );
    let mut reconnected = connect(&server.url, "b", "b-token").await;
    registered(&mut reconnected, true).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), reconnected.output.message())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn replays_unacknowledged_dispatch_after_server_restart() {
    let (server, _temp, config) = setup().await;
    let mut a = connect(&server.url, "a", "a-token").await;
    let mut b = connect(&server.url, "b", "b-token").await;
    registered(&mut a, true).await;
    registered(&mut b, true).await;
    a.input
        .send(create("restart", "a-restart", None))
        .await
        .unwrap();
    let delivered = b.output.message().await.unwrap().unwrap();
    let core_to_connector::Operation::DispatchCreateMessage(delivered) =
        delivered.operation.unwrap()
    else {
        panic!("expected initial dispatch");
    };
    drop(a);
    drop(b);
    server.stop().await;

    let restarted = spawn_server(config).await;
    let mut b = connect(&restarted.url, "b", "b-token").await;
    registered(&mut b, true).await;
    let replayed = b.output.message().await.unwrap().unwrap();
    let core_to_connector::Operation::DispatchCreateMessage(replayed) = replayed.operation.unwrap()
    else {
        panic!("expected restarted delivery");
    };
    assert_eq!(replayed.dispatch_id, delivered.dispatch_id);
}
