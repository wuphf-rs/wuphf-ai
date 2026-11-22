//! The SDK's runtime settings against a scripted botconf.v1.Settings service,
//! and the registration that carries the declaration, against a recording
//! core.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use botconf_proto as bc;
use botconf_proto::settings_server::{Settings, SettingsServer};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use wuphf_connector::settings::{Backoff, Connection, RuntimeSettings, Setting, SettingKind};
use wuphf_connector::{Client, Delivery, Failure, Options, Sender};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_server::{
    ConnectorTransport, ConnectorTransportServer,
};

const TOKEN: &str = "wa-token";
const CONNECTOR: &str = "whatsapp-rust";
const PREFIX: &str = "connector_settings.whatsapp-rust";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Section {
    limit: u32,
    verbose: bool,
}

impl RuntimeSettings for Section {
    fn settings() -> Vec<Setting> {
        vec![
            Setting {
                key: "limit",
                title: "Limit",
                description: "At most",
                kind: SettingKind::Number { min: 0, max: 100 },
            },
            Setting {
                key: "verbose",
                title: "Verbose",
                description: "Say more",
                kind: SettingKind::Bool,
            },
        ]
    }
}

fn base() -> Section {
    Section {
        limit: 1,
        verbose: false,
    }
}

/// One watch stream: its events, then the stream ends or stays open.
struct Script {
    events: Vec<bc::WatchEvent>,
    then_end: bool,
}

/// Each watch request the fake saw, with its authorization header.
type Seen = Arc<Mutex<Vec<(bc::WatchRequest, Option<String>)>>>;

/// A settings service whose watch streams follow a script, one per
/// connection; every other call is unimplemented.
#[derive(Clone, Default)]
struct FakeSettings {
    scripts: Arc<Mutex<VecDeque<Script>>>,
    seen: Seen,
}

type EventStream = Pin<Box<dyn Stream<Item = Result<bc::WatchEvent, Status>> + Send>>;

fn unimplemented() -> Status {
    Status::unimplemented("not in this test")
}

#[tonic::async_trait]
impl Settings for FakeSettings {
    type WatchStream = EventStream;

    async fn watch(
        &self,
        request: Request<bc::WatchRequest>,
    ) -> Result<Response<EventStream>, Status> {
        let authorization = request
            .metadata()
            .get("authorization")
            .map(|value| value.to_str().unwrap().to_owned());
        self.seen
            .lock()
            .unwrap()
            .push((request.into_inner(), authorization));
        let script = self.scripts.lock().unwrap().pop_front().unwrap_or(Script {
            events: Vec::new(),
            then_end: false,
        });
        let events = tokio_stream::iter(script.events.into_iter().map(Ok));
        let stream: EventStream = if script.then_end {
            Box::pin(events)
        } else {
            Box::pin(events.chain(tokio_stream::pending()))
        };
        Ok(Response::new(stream))
    }

    async fn describe(
        &self,
        _: Request<bc::DescribeRequest>,
    ) -> Result<Response<bc::DescribeResponse>, Status> {
        Err(unimplemented())
    }

    async fn list(
        &self,
        _: Request<bc::ListRequest>,
    ) -> Result<Response<bc::ListResponse>, Status> {
        Err(unimplemented())
    }

    async fn get(&self, _: Request<bc::GetRequest>) -> Result<Response<bc::ValueEntry>, Status> {
        Err(unimplemented())
    }

    async fn set(
        &self,
        _: Request<bc::EditRequest>,
    ) -> Result<Response<bc::ChangeResponse>, Status> {
        Err(unimplemented())
    }

    async fn unset(
        &self,
        _: Request<bc::UnsetRequest>,
    ) -> Result<Response<bc::ChangeResponse>, Status> {
        Err(unimplemented())
    }

    async fn add(
        &self,
        _: Request<bc::EditRequest>,
    ) -> Result<Response<bc::ChangeResponse>, Status> {
        Err(unimplemented())
    }

    async fn remove(
        &self,
        _: Request<bc::EditRequest>,
    ) -> Result<Response<bc::ChangeResponse>, Status> {
        Err(unimplemented())
    }

    async fn reload(
        &self,
        _: Request<bc::ReloadRequest>,
    ) -> Result<Response<bc::ChangeResponse>, Status> {
        Err(unimplemented())
    }

    async fn render(
        &self,
        _: Request<bc::RenderRequest>,
    ) -> Result<Response<bc::RenderResponse>, Status> {
        Err(unimplemented())
    }

    async fn act(&self, _: Request<bc::ActRequest>) -> Result<Response<bc::ActResponse>, Status> {
        Err(unimplemented())
    }
}

async fn serve(settings: FakeSettings) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(SettingsServer::new(settings))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (address, task)
}

fn value(key: &str, json: &str) -> bc::ValueEntry {
    bc::ValueEntry {
        key: format!("{PREFIX}.{key}"),
        value_json: Some(json.into()),
        ..Default::default()
    }
}

fn event(initial: bool, values: Vec<bc::ValueEntry>) -> bc::WatchEvent {
    bc::WatchEvent {
        values,
        initial,
        ..Default::default()
    }
}

/// Follows `connection` in the background, sending each change it reports.
fn follow(
    connection: Arc<Connection<Section>>,
) -> (
    mpsc::UnboundedReceiver<(Section, Section)>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let (changes_tx, changes) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            connection
                .follow(cancel, move |old: &Section, new: &Section| {
                    changes_tx.send((old.clone(), new.clone())).unwrap();
                })
                .await;
        }
    });
    (changes, cancel, task)
}

async fn next_change(
    changes: &mut mpsc::UnboundedReceiver<(Section, Section)>,
) -> (Section, Section) {
    tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("a change within 5 s")
        .unwrap()
}

fn connect(address: &str) -> Arc<Connection<Section>> {
    Arc::new(
        Connection::connect(address, TOKEN, CONNECTOR, base())
            .unwrap()
            .with_backoff(Backoff {
                initial: Duration::from_millis(10),
                max: Duration::from_millis(40),
            }),
    )
}

#[tokio::test]
async fn follow_applies_the_core_s_pushes() {
    let settings = FakeSettings::default();
    settings.scripts.lock().unwrap().push_back(Script {
        events: vec![
            event(true, vec![value("limit", "5")]),
            event(false, vec![value("verbose", "true")]),
            // Does not decode into a number: skipped.
            event(false, vec![value("limit", "\"many\"")]),
            event(false, vec![value("limit", "7")]),
        ],
        then_end: false,
    });
    let (address, server) = serve(settings.clone()).await;
    let connection = connect(&address);
    let (mut changes, cancel, task) = follow(Arc::clone(&connection));

    let (old, new) = next_change(&mut changes).await;
    assert_eq!((old, new.clone()), (base(), Section { limit: 5, ..base() }));
    let (_, new) = next_change(&mut changes).await;
    assert_eq!(
        new,
        Section {
            limit: 5,
            verbose: true
        }
    );
    // The undecodable push changed nothing; the next one applies.
    let (old, new) = next_change(&mut changes).await;
    assert_eq!(
        old,
        Section {
            limit: 5,
            verbose: true
        }
    );
    assert_eq!(
        new,
        Section {
            limit: 7,
            verbose: true
        }
    );
    assert_eq!(*connection.live.load(), new);

    let seen = settings.seen.lock().unwrap().clone();
    let (request, authorization) = &seen[0];
    assert_eq!(authorization.as_deref(), Some("Bearer wa-token"));
    assert_eq!(request.actor, None);
    assert_eq!(request.prefixes, [PREFIX]);
    cancel.cancel();
    task.await.unwrap();
    server.abort();
}

#[tokio::test]
async fn follow_reconnects_when_the_stream_ends() {
    let settings = FakeSettings::default();
    {
        let mut scripts = settings.scripts.lock().unwrap();
        scripts.push_back(Script {
            events: vec![event(true, vec![value("limit", "5")])],
            then_end: true,
        });
        // The reconnect starts again from the base.
        scripts.push_back(Script {
            events: vec![event(true, vec![value("verbose", "true")])],
            then_end: false,
        });
    }
    let (address, server) = serve(settings.clone()).await;
    let connection = connect(&address);
    let (mut changes, cancel, task) = follow(Arc::clone(&connection));

    let (_, first) = next_change(&mut changes).await;
    assert_eq!(first, Section { limit: 5, ..base() });
    let (_, second) = next_change(&mut changes).await;
    assert_eq!(
        second,
        Section {
            verbose: true,
            ..base()
        }
    );
    assert_eq!(settings.seen.lock().unwrap().len(), 2);
    cancel.cancel();
    task.await.unwrap();
    server.abort();
}

/// A core that records each session's registration and refuses the session.
#[derive(Clone, Default)]
struct RecordingCore {
    registrations: Arc<Mutex<Vec<pb::Registration>>>,
}

#[tonic::async_trait]
impl ConnectorTransport for RecordingCore {
    type OpenSessionStream = Pin<Box<dyn Stream<Item = Result<pb::ServerFrame, Status>> + Send>>;

    async fn open_session(
        &self,
        request: Request<tonic::Streaming<pb::ClientFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        let mut frames = request.into_inner();
        if let Some(pb::ClientFrame {
            body: Some(pb::client_frame::Body::Registration(registration)),
        }) = frames.message().await?
        {
            self.registrations.lock().unwrap().push(registration);
        }
        Err(Status::unavailable("recorded"))
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

#[tokio::test]
async fn the_registration_carries_the_declaration() {
    let core = RecordingCore::default();
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
    let declaration = wuphf_connector::settings::declare(&base()).unwrap();
    let client = Client::new(Options {
        core_address: address,
        connector_id: CONNECTOR.into(),
        platform: pb::Platform::Whatsapp,
        supports_threads: false,
        supports_topic_create: false,
        supports_disappearing_timers: false,
        token: TOKEN.into(),
        is_permanent: None,
        settings: Some(declaration.clone()),
    });
    let (_inputs, inputs_rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let session = tokio::spawn({
        let cancel = cancel.clone();
        async move { client.run(NoSends, inputs_rx, cancel).await }
    });

    let mut registration = None;
    for _ in 0..250 {
        registration = core.registrations.lock().unwrap().first().cloned();
        if registration.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let registration = registration.expect("the core saw a registration");
    assert_eq!(registration.settings, Some(declaration));
    cancel.cancel();
    session.await.unwrap().unwrap();
    server.abort();
}
