use super::*;
use crate::auth::any_token_auth;
use crate::connector_settings::ConnectorSettings;
use botconf::SettingsError;
use botconf_grpc::proto;
use prost::Message;
use serde_json::json;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use wuphf_model::ConnectorId;
use wuphf_proto::connector::plugin_transport_client::PluginTransportClient;
use wuphf_proto::connector::plugin_transport_server::PluginTransportServer;
use wuphf_settings::{WuphfSchema, base, load};
use wuphf_storage::InMemoryStorage;

const MISC: &str = "misc";
const OBSERVER_TOKEN: &str = "observer-secret";

const CONFIG: &str = r#"
listen = "127.0.0.1:50051"
database_url = "sqlite://unused.sqlite"
"#;

/// wuphf-misc's runtime settings, as its config's `Runtime` declares them.
const SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "log_level": {"type": "string", "enum": ["trace", "debug", "info", "warn", "error"]},
        "instagram": {
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean"},
                "chats": {"type": "array", "items": {"type": "string"}},
                "max_per_minute": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }
    },
    "additionalProperties": false
}"#;

fn config_file(toml: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "wuphf-plugin-settings-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, toml).unwrap();
    path
}

async fn store_with(toml: &str, overrides: Arc<botconf::MemoryStorage>) -> Arc<SettingsStore> {
    let schema = WuphfSchema::with_env(|name| Some(format!("secret-{name}")));
    Arc::new(
        load(schema, base(&config_file(toml)).unwrap(), overrides)
            .await
            .unwrap(),
    )
}

async fn store() -> Arc<SettingsStore> {
    store_with(CONFIG, Arc::new(botconf::MemoryStorage::new())).await
}

fn described(key: &str) -> proto::SettingDescriptor {
    proto::SettingDescriptor {
        key: key.into(),
        description: format!("{key}."),
        ..Default::default()
    }
}

fn declaration_of(plugin: &str, keys: &[&str]) -> SettingsDeclaration {
    SettingsDeclaration {
        owner: SettingsOwner::Plugin(plugin.into()),
        descriptor: proto::DescribeResponse {
            settings: keys.iter().map(|key| described(key)).collect(),
            sections: Vec::new(),
        }
        .encode_to_vec(),
        json_schema: SCHEMA.into(),
        base_json: json!({
            "log_level": "info",
            "instagram": {"enabled": false, "chats": [], "max_per_minute": 10}
        })
        .to_string(),
    }
}

fn declaration() -> SettingsDeclaration {
    declaration_of(
        MISC,
        &[
            "log_level",
            "instagram.enabled",
            "instagram.chats",
            "instagram.max_per_minute",
        ],
    )
}

fn key(rest: &str) -> String {
    format!("{}.{rest}", section_path(MISC))
}

#[tokio::test]
async fn declaration_creates_a_section_over_the_plugin_base() {
    let store = store().await;
    PluginSettings::new(Arc::clone(&store), Arc::new(InMemoryStorage::default()))
        .declare(declaration())
        .await
        .unwrap();

    let catalog = store.catalog();
    let entry = catalog.resolve(&key("instagram.enabled")).unwrap();
    assert_eq!(entry.section.as_deref(), Some("plugin:misc"));
    let snapshot = store.current();
    assert_eq!(
        snapshot.value(&key("instagram.enabled")),
        Some(json!(false))
    );
    assert_eq!(
        snapshot.source(&key("instagram.enabled")),
        botconf::Source::Other("plugin misc".into())
    );

    // A panel's text values are read as the schema's types.
    store
        .set(&key("instagram.enabled"), json!("true"), None)
        .await
        .unwrap();
    store
        .set(
            &key("instagram.chats"),
            json!(["whatsapp-airtel:1@g.us"]),
            None,
        )
        .await
        .unwrap();
    let snapshot = store.current();
    assert_eq!(snapshot.value(&key("instagram.enabled")), Some(json!(true)));
    assert_eq!(
        snapshot.value(&key("instagram.chats")),
        Some(json!(["whatsapp-airtel:1@g.us"]))
    );

    let error = store
        .set(&key("instagram.max_per_minute"), json!(-1), None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SettingsError::InvalidValue { .. }),
        "{error}"
    );
}

#[tokio::test]
async fn connection_keys_and_bad_ids_are_refused() {
    for refused in [
        "core_address",
        "plugin_id",
        "observer_token_env",
        "invoke_token_env",
        "instagram.token_env",
    ] {
        let error = section_for(&declaration_of(MISC, &["log_level", refused])).unwrap_err();
        assert!(error.contains(refused), "{refused}: {error}");
    }
    // Only a whole key part counts.
    assert!(section_for(&declaration_of(MISC, &["core_addresses"])).is_ok());

    for id in ["", "Misc", "my misc", "misc:1", "1misc"] {
        let error = section_for(&declaration_of(id, &["log_level"])).unwrap_err();
        assert!(error.contains("plugin id"), "{id:?}: {error}");
    }
    let connector = SettingsDeclaration {
        owner: SettingsOwner::Connector(ConnectorId(MISC.into())),
        ..declaration()
    };
    assert!(section_for(&connector).is_err());
}

#[tokio::test]
async fn declaration_survives_restart_and_restores_apart_from_connectors() {
    let overrides = Arc::new(botconf::MemoryStorage::new());
    let declarations = Arc::new(InMemoryStorage::default());
    let first = store_with(CONFIG, Arc::clone(&overrides)).await;
    PluginSettings::new(Arc::clone(&first), declarations.clone())
        .declare(declaration())
        .await
        .unwrap();
    first
        .set(&key("instagram.enabled"), json!(true), None)
        .await
        .unwrap();

    let second = store_with(CONFIG, overrides).await;
    assert_eq!(second.current().value(&key("instagram.enabled")), None);
    // Connectors' restore leaves plugins' declarations alone (and does not
    // warn about them as unconfigured connectors).
    ConnectorSettings::new(Arc::clone(&second), declarations.clone())
        .restore()
        .await
        .unwrap();
    assert_eq!(second.current().value(&key("instagram.enabled")), None);
    PluginSettings::new(Arc::clone(&second), declarations)
        .restore()
        .await
        .unwrap();
    assert_eq!(
        second.current().value(&key("instagram.enabled")),
        Some(json!(true))
    );
}

#[tokio::test]
async fn core_file_cannot_set_plugin_settings() {
    let toml = format!("{CONFIG}\n[plugin_settings.misc]\nlog_level = \"debug\"\n");
    let schema = WuphfSchema::with_env(|name| Some(format!("secret-{name}")));
    let error = load(
        schema,
        base(&config_file(&toml)).unwrap(),
        botconf::MemoryStorage::new(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("plugin_settings"), "{error}");
}

// ---- over gRPC ----

/// The plugin transport behind the observer token `observer` (none refuses
/// everything), and the store it declares into.
async fn serve(
    observer: Option<&str>,
) -> (String, Arc<SettingsStore>, tokio::task::JoinHandle<()>) {
    let store = store().await;
    let settings = Arc::new(PluginSettings::new(
        Arc::clone(&store),
        Arc::new(InMemoryStorage::default()),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = PluginTransportServer::with_interceptor(
        GrpcPluginService::new(settings),
        any_token_auth(observer.map(str::to_owned).into_iter().collect()),
    );
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (format!("http://{address}"), store, task)
}

async fn declare(
    uri: &str,
    token: Option<&str>,
    declaration: SettingsDeclaration,
) -> Result<(), tonic::Status> {
    let mut client = PluginTransportClient::connect(uri.to_owned())
        .await
        .unwrap();
    let mut request = Request::new(pb::DeclarePluginSettingsRequest {
        plugin_id: declaration.owner.id().to_owned(),
        settings: Some(pb::ConnectorSettingsDeclaration {
            descriptor: declaration.descriptor,
            json_schema: declaration.json_schema,
            base_json: declaration.base_json,
        }),
    });
    if let Some(token) = token {
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    client.declare_settings(request).await.map(drop)
}

#[tokio::test]
async fn the_observer_token_declares_a_plugin_section() {
    let (uri, store, task) = serve(Some(OBSERVER_TOKEN)).await;

    declare(&uri, Some(OBSERVER_TOKEN), declaration())
        .await
        .unwrap();
    assert_eq!(
        store.current().value(&key("instagram.enabled")),
        Some(json!(false))
    );

    let refused = declare(
        &uri,
        Some(OBSERVER_TOKEN),
        declaration_of(MISC, &["core_address"]),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument, "{refused}");
    task.abort();
}

#[tokio::test]
async fn declaring_needs_the_observer_token() {
    let (uri, store, task) = serve(Some(OBSERVER_TOKEN)).await;
    for token in [None, Some(""), Some("wrong")] {
        let error = declare(&uri, token, declaration()).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unauthenticated, "{token:?}");
    }
    assert_eq!(store.current().value(&key("instagram.enabled")), None);
    task.abort();

    // With no observer token set, nothing is accepted.
    let (uri, _store, task) = serve(None).await;
    let error = declare(&uri, Some(OBSERVER_TOKEN), declaration())
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    task.abort();
}
