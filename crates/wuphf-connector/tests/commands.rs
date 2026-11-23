//! The owner's commands against a scripted core: the registration declares
//! them, and each RunCommand is answered with its CommandResult.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use wuphf_connector::commands::BoxFuture;
use wuphf_connector::{Client, Commands, Delivery, Failure, Options, Sender};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_server::{
    ConnectorTransport, ConnectorTransportServer,
};

/// A core that records the registration and every later client frame, and
/// sends the scripted RunCommand frames once the session opens.
#[derive(Clone, Default)]
struct ScriptedCore {
    registrations: Arc<Mutex<Vec<pb::Registration>>>,
    frames: Arc<Mutex<Vec<pb::ClientFrame>>>,
    script: Vec<pb::RunCommand>,
}

type Frames = Pin<Box<dyn Stream<Item = Result<pb::ServerFrame, Status>> + Send>>;

#[tonic::async_trait]
impl ConnectorTransport for ScriptedCore {
    type OpenSessionStream = Frames;

    async fn open_session(
        &self,
        request: Request<tonic::Streaming<pb::ClientFrame>>,
    ) -> Result<Response<Frames>, Status> {
        let mut incoming = request.into_inner();
        if let Some(pb::ClientFrame {
            body: Some(pb::client_frame::Body::Registration(registration)),
        }) = incoming.message().await?
        {
            self.registrations.lock().unwrap().push(registration);
        }
        let frames = Arc::clone(&self.frames);
        tokio::spawn(async move {
            while let Ok(Some(frame)) = incoming.message().await {
                frames.lock().unwrap().push(frame);
            }
        });
        let (tx, rx) = mpsc::channel(8);
        for command in self.script.clone() {
            tx.send(Ok(pb::ServerFrame {
                body: Some(pb::server_frame::Body::RunCommand(command)),
            }))
            .await
            .unwrap();
        }
        // Keep the stream open.
        tokio::spawn(async move {
            tx.closed().await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

struct NoSends;

impl Sender for NoSends {
    async fn send_message(&self, _: pb::MessageCreateDispatch) -> Result<Delivery, Failure> {
        Err(Failure::permanent("not in this test"))
    }

    async fn edit_message(&self, _: pb::MessageEditDispatch) -> Result<String, Failure> {
        Err(Failure::permanent("not in this test"))
    }

    async fn delete_message(&self, _: pb::MessageDeleteDispatch) -> Result<String, Failure> {
        Err(Failure::permanent("not in this test"))
    }
}

/// `ping` answers `pong`; `fail` fails.
struct Ping;

impl Commands for Ping {
    fn declarations(&self) -> Vec<pb::ConnectorCommand> {
        ["ping", "fail"]
            .into_iter()
            .map(|id| pb::ConnectorCommand {
                id: id.into(),
                title: format!("Do {id}"),
            })
            .collect()
    }

    fn run<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            match id {
                "ping" => Ok("pong".to_owned()),
                _ => Err("down".to_owned()),
            }
        })
    }
}

fn run(id: &str, request: &str) -> pb::RunCommand {
    pb::RunCommand {
        request_id: request.into(),
        command_id: id.into(),
    }
}

/// Runs a session with `commands` against a core sending `script`, until
/// `results` command results arrived.
async fn session(
    commands: Option<Arc<dyn Commands>>,
    script: Vec<pb::RunCommand>,
    results: usize,
) -> (Vec<pb::Registration>, Vec<pb::CommandResult>) {
    let core = ScriptedCore {
        script,
        ..ScriptedCore::default()
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn({
        let core = core.clone();
        async move {
            Server::builder()
                .add_service(ConnectorTransportServer::new(core))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        }
    });
    let client = Client::new(Options {
        core_address: address,
        connector_id: "whatsapp-rust".into(),
        platform: pb::Platform::Whatsapp,
        supports_threads: false,
        supports_topic_create: false,
        supports_disappearing_timers: false,
        token: "t".into(),
        is_permanent: None,
        settings: None,
        commands,
    });
    let (_inputs, inputs_rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let session = tokio::spawn({
        let cancel = cancel.clone();
        async move { client.run(NoSends, inputs_rx, cancel).await }
    });

    let answered = || -> Vec<pb::CommandResult> {
        core.frames
            .lock()
            .unwrap()
            .iter()
            .filter_map(|frame| match &frame.body {
                Some(pb::client_frame::Body::CommandResult(result)) => Some(result.clone()),
                _ => None,
            })
            .collect()
    };
    for _ in 0..250 {
        if answered().len() >= results && !core.registrations.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cancel.cancel();
    session.await.unwrap().unwrap();
    server.abort();
    let mut results = answered();
    results.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    let registrations = core.registrations.lock().unwrap().clone();
    (registrations, results)
}

fn result(request: &str, ok: bool, message: &str) -> pb::CommandResult {
    pb::CommandResult {
        request_id: request.into(),
        ok,
        message: message.into(),
    }
}

#[tokio::test]
async fn the_registration_declares_the_commands() {
    let (registrations, _) = session(Some(Arc::new(Ping)), Vec::new(), 0).await;
    let declared: Vec<(String, String)> = registrations[0]
        .commands
        .iter()
        .map(|command| (command.id.clone(), command.title.clone()))
        .collect();
    assert_eq!(
        declared,
        [
            ("ping".into(), "Do ping".into()),
            ("fail".into(), "Do fail".into())
        ]
    );
}

#[tokio::test]
async fn each_command_is_answered() {
    let script = vec![run("ping", "r1"), run("fail", "r2"), run("nope", "r3")];
    let (_, results) = session(Some(Arc::new(Ping)), script, 3).await;
    assert_eq!(
        results,
        [
            result("r1", true, "pong"),
            result("r2", false, "down"),
            result("r3", false, "unknown command \"nope\""),
        ]
    );
}

#[tokio::test]
async fn a_connector_without_commands_refuses_them() {
    let (registrations, results) = session(None, vec![run("ping", "r1")], 1).await;
    assert!(registrations[0].commands.is_empty());
    assert_eq!(
        results,
        [result("r1", false, "this connector runs no commands")]
    );
}
