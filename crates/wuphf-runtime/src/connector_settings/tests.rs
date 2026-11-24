use super::*;
use crate::RuntimeChannels;
use crate::auth::ConnectorTokens;
use crate::connector::ConnectorHub;
use crate::connector_commands::ConnectorCommands;
use crate::declared_settings::kind_from_wire;
use crate::grpc::GrpcConnectorService;
use botconf::Kind;
use botconf::SettingsError;
use botconf_grpc::proto;
use botconf_panel::{Limits, Page, Panel, Viewer};
use prost::Message;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Server;
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_client::ConnectorTransportClient;
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_settings::{WuphfSchema, base, load};
use wuphf_storage::InMemoryStorage;

const TG: &str = "telegram-main";
const TG_TOKEN: &str = "secret-TELEGRAM_TOKEN";

const CONFIG: &str = r#"
listen = "127.0.0.1:50051"
database_url = "sqlite://unused.sqlite"

[[connectors]]
id = "telegram-main"
platform = "telegram"
token_env = "TELEGRAM_TOKEN"
"#;

fn config_file(toml: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "wuphf-connector-settings-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, toml).unwrap();
    path
}

/// A settings store over `toml`, every token variable `secret-<name>`, with
/// its overrides in `overrides`.
async fn store_with(toml: &str, overrides: Arc<botconf::MemoryStorage>) -> Arc<SettingsStore> {
    let schema = WuphfSchema::with_env(|name| Some(format!("secret-{name}")));
    let store = load(schema, base(&config_file(toml)).unwrap(), overrides)
        .await
        .unwrap();
    Arc::new(store)
}

async fn store() -> Arc<SettingsStore> {
    store_with(CONFIG, Arc::new(botconf::MemoryStorage::new())).await
}

fn choice(value: &str) -> proto::Choice {
    proto::Choice {
        value: value.into(),
        label: value.into(),
    }
}

fn wire(kind: proto::kind::Kind) -> Option<proto::Kind> {
    Some(proto::Kind { kind: Some(kind) })
}

fn described(key: &str, kind: Option<proto::Kind>) -> proto::SettingDescriptor {
    proto::SettingDescriptor {
        key: key.into(),
        title: String::new(),
        description: format!("the {key}"),
        kind,
        ..Default::default()
    }
}

/// The Telegram connector's settings: an edit mode from a fixed list, a
/// message length, and a log level.
fn telegram_settings() -> Vec<proto::SettingDescriptor> {
    use proto::kind::Kind as Wire;
    vec![
        described(
            "rendering.edit_mode",
            wire(Wire::OneOf(proto::OneOfKind {
                choices: vec![choice("in_place"), choice("repost")],
                custom: false,
                optional: false,
            })),
        ),
        described(
            "rendering.max_message_length",
            wire(Wire::Number(proto::NumberKind {
                min: 1,
                max: 4096,
                ..Default::default()
            })),
        ),
        described(
            "rendering.attachments_as_files",
            wire(Wire::OneOf(proto::OneOfKind {
                choices: vec![choice("true"), choice("false")],
                custom: false,
                optional: false,
            })),
        ),
        described("log_level", wire(Wire::Text(proto::TextKind::default()))),
    ]
}

const SCHEMA: &str = r#"{
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "properties": {
        "log_level": { "type": "string" },
        "rendering": {
            "type": "object",
            "properties": {
                "edit_mode": { "type": "string", "enum": ["in_place", "repost"] },
                "max_message_length": { "type": "integer", "minimum": 1 },
                "attachments_as_files": { "type": "boolean" }
            },
            "required": ["edit_mode", "max_message_length", "attachments_as_files"],
            "additionalProperties": false
        }
    },
    "required": ["log_level", "rendering"],
    "additionalProperties": false
}"#;

fn declaration_of(settings: Vec<proto::SettingDescriptor>) -> SettingsDeclaration {
    SettingsDeclaration {
        owner: SettingsOwner::Connector(ConnectorId(TG.into())),
        descriptor: proto::DescribeResponse {
            settings,
            sections: Vec::new(),
        }
        .encode_to_vec(),
        json_schema: SCHEMA.into(),
        base_json: json!({
            "log_level": "info",
            "rendering": {
                "edit_mode": "in_place",
                "max_message_length": 4096,
                "attachments_as_files": false
            }
        })
        .to_string(),
    }
}

fn declaration() -> SettingsDeclaration {
    declaration_of(telegram_settings())
}

fn edit_mode() -> String {
    format!(
        "{}.rendering.edit_mode",
        section_path(&ConnectorId(TG.into()))
    )
}

async fn declared(store: &Arc<SettingsStore>) -> ConnectorSettings {
    let settings = ConnectorSettings::new(Arc::clone(store), Arc::new(InMemoryStorage::default()));
    settings.declare(declaration()).await.unwrap();
    settings
}

#[tokio::test]
async fn declaration_creates_a_section() {
    let store = store().await;
    declared(&store).await;

    let catalog = store.catalog();
    let entry = catalog.resolve(&edit_mode()).unwrap();
    assert_eq!(entry.section.as_deref(), Some("connector:telegram-main"));
    let snapshot = store.current();
    assert_eq!(snapshot.value(&edit_mode()), Some(json!("in_place")));
    assert_eq!(
        snapshot.source(&edit_mode()),
        botconf::Source::Other("connector telegram-main".into())
    );
}

#[tokio::test]
async fn value_failing_the_schema_is_rejected() {
    let store = store().await;
    declared(&store).await;

    let error = store
        .set(&edit_mode(), json!("sideways"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SettingsError::InvalidValue { .. }),
        "{error}"
    );
    assert!(store.current().overrides().is_empty());
    assert_eq!(store.current().value(&edit_mode()), Some(json!("in_place")));
}

#[tokio::test]
async fn panel_text_values_are_read_as_the_schema_types() {
    let store = store().await;
    declared(&store).await;
    let path = section_path(&ConnectorId(TG.into()));

    // A panel choice arrives as text.
    store
        .set(
            &format!("{path}.rendering.attachments_as_files"),
            json!("true"),
            None,
        )
        .await
        .unwrap();
    store
        .set(
            &format!("{path}.rendering.max_message_length"),
            json!("1000"),
            None,
        )
        .await
        .unwrap();
    let snapshot = store.current();
    assert_eq!(
        snapshot.value(&format!("{path}.rendering.attachments_as_files")),
        Some(json!(true))
    );
    assert_eq!(
        snapshot.value(&format!("{path}.rendering.max_message_length")),
        Some(json!(1000))
    );
    let error = store
        .set(
            &format!("{path}.rendering.attachments_as_files"),
            json!("maybe"),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, SettingsError::InvalidValue { .. }),
        "{error}"
    );
}

#[tokio::test]
async fn restart_only_keys_are_refused_by_the_section() {
    let store = store().await;
    let settings = ConnectorSettings::new(Arc::clone(&store), Arc::new(InMemoryStorage::default()));
    let mut described = telegram_settings();
    described.push(super::tests::described("session_file", None));
    let error = settings.section(&declaration_of(described)).unwrap_err();
    assert!(error.contains("session_file"), "{error}");
    // `rendering_extra` is not below `rendering`.
    assert!(!is_live_safe("rendering_extra"));
    assert!(is_live_safe("rendering.edit_mode"));
}

#[tokio::test]
async fn unconfigured_connectors_cannot_declare() {
    let store = store().await;
    let settings = ConnectorSettings::new(Arc::clone(&store), Arc::new(InMemoryStorage::default()));
    let mut declaration = declaration();
    declaration.owner = SettingsOwner::Connector(ConnectorId("ghost".into()));
    let error = settings.declare(declaration).await.unwrap_err();
    assert!(error.contains("not configured"), "{error}");
}

#[tokio::test]
async fn declaration_survives_restart() {
    let overrides = Arc::new(botconf::MemoryStorage::new());
    let declarations = Arc::new(InMemoryStorage::default());
    let first = store_with(CONFIG, Arc::clone(&overrides)).await;
    ConnectorSettings::new(Arc::clone(&first), declarations.clone())
        .declare(declaration())
        .await
        .unwrap();
    first
        .set(&edit_mode(), json!("repost"), None)
        .await
        .unwrap();

    // A new store over the same storage: the override waits for its section
    // until the stored declaration is restored, before any connector returns.
    let second = store_with(CONFIG, overrides).await;
    assert_eq!(second.current().value(&edit_mode()), None);
    ConnectorSettings::new(Arc::clone(&second), declarations)
        .restore()
        .await
        .unwrap();
    assert_eq!(second.current().value(&edit_mode()), Some(json!("repost")));
}

#[tokio::test]
async fn unset_returns_to_the_connector_base() {
    let store = store().await;
    declared(&store).await;
    store
        .set(&edit_mode(), json!("repost"), None)
        .await
        .unwrap();
    assert_eq!(store.current().value(&edit_mode()), Some(json!("repost")));

    store.unset(&edit_mode()).await.unwrap();
    assert_eq!(store.current().value(&edit_mode()), Some(json!("in_place")));
}

#[tokio::test]
async fn watch_reports_connector_section_changes() {
    let store = store().await;
    declared(&store).await;
    let mut published = store.subscribe();
    published.borrow_and_update();

    store
        .set(&edit_mode(), json!("repost"), None)
        .await
        .unwrap();
    published.changed().await.unwrap();
    assert_eq!(
        published.borrow().value(&edit_mode()),
        Some(json!("repost"))
    );
}

#[tokio::test]
async fn core_file_cannot_set_connector_settings() {
    let toml = format!("{CONFIG}\n[connector_settings.telegram-main]\nlog_level = \"debug\"\n");
    let schema = WuphfSchema::with_env(|name| Some(format!("secret-{name}")));
    let error = load(
        schema,
        base(&config_file(&toml)).unwrap(),
        botconf::MemoryStorage::new(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("connector_settings"), "{error}");
}

// ---- over gRPC ----

struct Served {
    uri: String,
    store: Arc<SettingsStore>,
    declarations: Arc<InMemoryStorage>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Served {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// The connector transport over a hub allowing the configured connectors,
/// accepting settings declarations.
async fn serve() -> Served {
    serve_with(None).await
}

/// As [`serve`], offering live sessions' commands through `commands`.
async fn serve_with(commands: Option<Arc<ConnectorCommands>>) -> Served {
    let store = store().await;
    let declarations = Arc::new(InMemoryStorage::default());
    let settings = Arc::new(ConnectorSettings::new(
        Arc::clone(&store),
        declarations.clone(),
    ));
    let (inbound, _inbound_rx) = mpsc::channel(8);
    let (acknowledgements, _acknowledgements_rx) = mpsc::channel(8);
    let (failures, _failures_rx) = mpsc::channel(8);
    let (_dispatches, dispatches) = mpsc::channel(8);
    // The receivers are leaked into the hub task's lifetime by moving them in.
    let keep = (_inbound_rx, _acknowledgements_rx, _failures_rx, _dispatches);
    let (hub, handle) = ConnectorHub::new(
        RuntimeChannels {
            inbound,
            acknowledgements,
            failures,
            dispatches,
        },
        store.current().derived.connectors.clone(),
    );
    let hub_task = tokio::spawn(async move {
        let _keep = keep;
        hub.run().await;
    });
    let tokens = Arc::new(ConnectorTokens::new(HashMap::from([(
        ConnectorId(TG.into()),
        TG_TOKEN.to_owned(),
    )])));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(ConnectorTransportServer::new(match commands {
                Some(commands) => GrpcConnectorService::new(handle, tokens)
                    .with_settings(settings)
                    .with_commands(commands),
                None => GrpcConnectorService::new(handle, tokens).with_settings(settings),
            }))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    Served {
        uri: format!("http://{address}"),
        store,
        declarations,
        tasks: vec![hub_task, server],
    }
}

/// Opens a Telegram session declaring `settings`; the returned sender keeps
/// it open.
async fn open(
    uri: &str,
    settings: Option<pb::ConnectorSettingsDeclaration>,
) -> Result<mpsc::Sender<pb::ClientFrame>, tonic::Status> {
    open_declaring(uri, settings, Vec::new())
        .await
        .map(|(frames, _)| frames)
}

/// As [`open`], declaring `commands` too, with the core's frames.
async fn open_declaring(
    uri: &str,
    settings: Option<pb::ConnectorSettingsDeclaration>,
    commands: Vec<pb::ConnectorCommand>,
) -> Result<
    (
        mpsc::Sender<pb::ClientFrame>,
        tonic::Streaming<pb::ServerFrame>,
    ),
    tonic::Status,
> {
    let mut client = ConnectorTransportClient::connect(uri.to_owned())
        .await
        .unwrap();
    let (frames, frames_rx) = mpsc::channel(8);
    frames
        .send(pb::ClientFrame {
            body: Some(pb::client_frame::Body::Registration(pb::Registration {
                connector_id: TG.into(),
                platform: pb::Platform::Telegram.into(),
                capabilities: Some(pb::Capabilities::default()),
                settings,
                commands,
            })),
        })
        .await
        .unwrap();
    let mut request = tonic::Request::new(ReceiverStream::new(frames_rx));
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {TG_TOKEN}").parse().unwrap(),
    );
    let server_frames = client.open_session(request).await?.into_inner();
    Ok((frames, server_frames))
}

fn on_wire(declaration: SettingsDeclaration) -> pb::ConnectorSettingsDeclaration {
    pb::ConnectorSettingsDeclaration {
        descriptor: declaration.descriptor,
        json_schema: declaration.json_schema,
        base_json: declaration.base_json,
    }
}

#[tokio::test]
async fn registration_with_a_declaration_registers_the_section() {
    let served = serve().await;
    let _session = open(&served.uri, Some(on_wire(declaration())))
        .await
        .unwrap();
    // The section is registered right after the session is accepted.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while served.store.catalog().resolve(&edit_mode()).is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the section is registered");
    assert_eq!(
        served.declarations.settings_declarations().await.unwrap(),
        [declaration()]
    );
}

#[tokio::test]
async fn a_live_session_s_commands_run_from_its_section() {
    let commands = Arc::new(ConnectorCommands::default());
    let served = serve_with(Some(Arc::clone(&commands))).await;
    let resync = pb::ConnectorCommand {
        id: "resync".into(),
        title: "🔄 Resync".into(),
    };
    let (frames, mut server_frames) =
        open_declaring(&served.uri, Some(on_wire(declaration())), vec![resync])
            .await
            .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while served.store.catalog().resolve(&edit_mode()).is_err()
            || commands.offered(TG).is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the section and its command are offered");

    let panel = crate::settings_auth::with_commands(
        Panel::new(Arc::clone(&served.store)),
        Arc::clone(&commands),
    );
    let limits = Limits {
        token_bytes: 64,
        text_chars: 4096,
        max_rows: None,
        max_per_row: 8,
    };
    let viewer = Viewer::new(&limits);
    let section = Page::Section(format!("connector:{TG}"));
    let screen = panel.render(&section, &viewer).await;
    let token = screen
        .rows
        .iter()
        .flatten()
        .find(|button| button.label == "🔄 Resync")
        .expect("the command's button")
        .token
        .clone();

    // The connector answers the press it is sent.
    let answering = tokio::spawn(async move {
        let frame = server_frames.message().await.unwrap().unwrap();
        let Some(pb::server_frame::Body::RunCommand(command)) = frame.body else {
            panic!("a RunCommand, not {frame:?}");
        };
        assert_eq!(command.command_id, "resync");
        frames
            .send(pb::ClientFrame {
                body: Some(pb::client_frame::Body::CommandResult(pb::CommandResult {
                    request_id: command.request_id,
                    ok: true,
                    message: "resyncing".into(),
                })),
            })
            .await
            .unwrap();
        (frames, server_frames)
    });
    let reaction = panel.press(&token, &viewer).await.unwrap();
    assert_eq!(reaction.notice.as_deref(), Some("resyncing"));

    // When the session ends, the button goes.
    drop(answering.await.unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !commands.offered(TG).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the command is withdrawn with its session");
    let screen = panel.render(&section, &viewer).await;
    assert!(
        !screen
            .rows
            .iter()
            .flatten()
            .any(|button| button.label == "🔄 Resync")
    );
}

#[tokio::test]
async fn restart_only_keys_are_refused() {
    let served = serve().await;
    let mut described = telegram_settings();
    described.push(super::tests::described("session_file", None));
    let error = open(&served.uri, Some(on_wire(declaration_of(described))))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    // The hub never saw the attempt: the connector still gets in.
    open(&served.uri, None)
        .await
        .expect("the connector registers without the bad declaration");
    assert!(
        served
            .declarations
            .settings_declarations()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn registration_without_declaration_keeps_the_section() {
    let served = serve().await;
    ConnectorSettings::new(Arc::clone(&served.store), served.declarations.clone())
        .declare(declaration())
        .await
        .unwrap();
    let _session = open(&served.uri, None).await.unwrap();
    assert!(served.store.catalog().resolve(&edit_mode()).is_ok());
    assert_eq!(
        served.declarations.settings_declarations().await.unwrap(),
        [declaration()]
    );
}

#[tokio::test]
async fn registration_equality_ignores_the_declaration() {
    let served = serve().await;
    // The configured entry has no declaration; a session declaring settings
    // still matches it.
    open(&served.uri, Some(on_wire(declaration())))
        .await
        .expect("the declaration is not part of the identity check");
}

// ---- the Go connectors' declarations ----

/// What the Telegram connector declares by default, written by its Go test
/// (`WUPHF_UPDATE_FIXTURES=1 go test ./internal/config/` in
/// connectors/telegram), so a schema or descriptor the Go side generates must
/// stay readable here.
const TELEGRAM_FIXTURE: &str =
    include_str!("../../../../connectors/telegram/testdata/settings_declaration.json");

fn base64_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0_u32;
    let mut count = 0;
    let mut out = Vec::new();
    for byte in text.bytes().filter(|byte| *byte != b'=') {
        let value = ALPHABET
            .iter()
            .position(|candidate| *candidate == byte)
            .expect("base64") as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    out
}

#[tokio::test]
async fn the_telegram_connectors_declaration_is_accepted() {
    let fixture: Value = serde_json::from_str(TELEGRAM_FIXTURE).unwrap();
    let field = |name: &str| fixture[name].as_str().unwrap().to_owned();
    let declaration = SettingsDeclaration {
        owner: SettingsOwner::Connector(ConnectorId(TG.into())),
        descriptor: base64_decode(&field("descriptor_base64")),
        json_schema: field("json_schema"),
        base_json: field("base_json"),
    };
    let store = store().await;
    ConnectorSettings::new(Arc::clone(&store), Arc::new(InMemoryStorage::default()))
        .declare(declaration)
        .await
        .unwrap();

    let path = section_path(&ConnectorId(TG.into()));
    assert_eq!(store.current().value(&edit_mode()), Some(json!("in_place")));
    store
        .set(&edit_mode(), json!("repost"), None)
        .await
        .unwrap();
    store
        .set(
            &format!("{path}.rendering.attachments_as_files"),
            json!("true"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .current()
            .value(&format!("{path}.rendering.attachments_as_files")),
        Some(json!(true))
    );
    for (key, value) in [
        ("rendering.edit_mode", json!("sideways")),
        ("log_level", json!("loud")),
    ] {
        let error = store
            .set(&format!("{path}.{key}"), value, None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, SettingsError::InvalidValue { .. }),
            "{error}"
        );
    }
}

#[test]
fn form_fields_keep_required_and_when_from_the_wire() {
    use proto::kind::Kind as Wire;
    let field = |key: &str, required: bool, when: Option<(&str, &str)>| proto::FormField {
        key: key.into(),
        title: key.into(),
        kind: wire(Wire::Text(proto::TextKind::default())),
        group: String::new(),
        required,
        when: when.map(|(key, value)| proto::FieldCondition {
            key: key.into(),
            value: value.into(),
        }),
    };
    let form = wire(Wire::Form(proto::FormKind {
        fields: vec![
            field("mode", true, None),
            field("target", true, Some(("mode", "direct"))),
            field("note", false, None),
        ],
        initial_json: "{}".into(),
    }))
    .unwrap();

    let Kind::Form(form) = kind_from_wire(form).unwrap() else {
        panic!("a form arrives as a form");
    };
    let fields: Vec<_> = form
        .fields
        .iter()
        .map(|field| {
            (
                field.key.to_string(),
                field.required,
                field
                    .when
                    .as_ref()
                    .map(|(key, value)| (key.to_string(), value.to_string())),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("mode".to_owned(), true, None),
            (
                "target".to_owned(),
                true,
                Some(("mode".to_owned(), "direct".to_owned()))
            ),
            ("note".to_owned(), false, None),
        ]
    );
}
