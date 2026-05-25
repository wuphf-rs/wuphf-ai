use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Server;
use wuphf_core::{Config, ConnectorService, Engine, Store};
use wuphf_proto::v1::{
    connector_client::ConnectorClient, connector_server::ConnectorServer, connector_to_core,
    core_to_connector, ConnectorToCore, CreateMessage, DispatchSucceeded, Register,
};

struct Client {
    input: mpsc::Sender<ConnectorToCore>,
    output: tonic::Streaming<wuphf_proto::v1::CoreToConnector>,
}

async fn setup() -> (String, TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("grpc.sqlite");
    let config: Config = toml::from_str(&format!(
        r#"
            [server]
            listen = "127.0.0.1:0"
            [database]
            url = "sqlite://{}"
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
    let store = Store::open(&config).await.unwrap();
    let service = ConnectorService::new(Engine::new(config, store));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        Server::builder()
            .add_service(ConnectorServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (format!("http://{address}"), temp)
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
    let (server, _temp) = setup().await;
    let mut a = connect(&server, "a", "a-token").await;
    let mut b = connect(&server, "b", "b-token").await;
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

    let mut reconnected = connect(&server, "b", "b-token").await;
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
    let (server, _temp) = setup().await;
    let mut client = connect(&server, "a", "wrong").await;
    registered(&mut client, false).await;
}
