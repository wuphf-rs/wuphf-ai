use super::*;
use botconf::cli::KeyValue;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use wuphf_runtime::auth::ConnectorTokens;
use wuphf_runtime::settings_auth::settings_server;

const OBSERVER_TOKEN: &str = "observer-secret";
const NOTIFY: &str = r#"["telegram-main:telegram:user:1"]"#;

/// A fresh temp path with `extension`.
fn temp_path(extension: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "wuphf-cli-settings-{}.{extension}",
        uuid::Uuid::new_v4()
    ))
}

/// The example config, with its database moved to a fresh temp file.
fn config_file() -> PathBuf {
    let database = temp_path("sqlite");
    let example = include_str!("../../../../config/example.toml");
    let toml = example.replace(
        "database_url = \"sqlite://wuphf.sqlite\"",
        &format!("database_url = \"sqlite://{}\"", database.display()),
    );
    assert_ne!(toml, example, "the example config names its database");
    let path = temp_path("toml");
    std::fs::write(&path, toml).unwrap();
    path
}

/// Every token variable reads as `secret-<name>`.
fn schema() -> WuphfSchema {
    WuphfSchema::with_env(|name| Some(format!("secret-{name}")))
}

async fn store(config: &Path) -> Arc<SettingsStore> {
    open(config, schema()).await.unwrap().1
}

fn set(key: &str, value: &str) -> SettingsAction {
    SettingsAction::Set(KeyValue {
        key: key.into(),
        value: vec![value.into()],
    })
}

/// The line `list` prints for `key`.
async fn listed(store: &SettingsStore, key: &str) -> String {
    let listing = run_local(store, SettingsAction::List).await.unwrap().output;
    listing
        .lines()
        .find(|line| line.starts_with(&format!("{key} = ")))
        .unwrap_or_else(|| panic!("{key} is not listed:\n{listing}"))
        .to_owned()
}

#[tokio::test]
async fn settings_set_then_list_shows_stored_value() {
    let config = config_file();
    let outcome = run_local(&*store(&config).await, set("notify", NOTIFY))
        .await
        .unwrap();
    assert!(outcome.changed, "{}", outcome.output);

    // A new store over the same database (the next command, or the core)
    // sees the change: it went to the database, not just to memory.
    let line = listed(&*store(&config).await, "notify").await;
    assert_eq!(line, format!("notify = {NOTIFY} (stored)"));
}

#[tokio::test]
async fn settings_set_rejects_invalid_value_and_stores_nothing() {
    let config = config_file();
    let error = run_local(
        &*store(&config).await,
        set("notify", r#"["no-such-connector:chat"]"#),
    )
    .await
    .err()
    .expect("a notify target on an unknown connector is refused");
    assert!(
        format!("{error:#}").contains("no-such-connector"),
        "{error:#}"
    );

    let reopened = store(&config).await;
    assert!(reopened.current().overrides().is_empty());
    assert!(listed(&reopened, "notify").await.ends_with("(config file)"));
}

#[tokio::test]
async fn settings_unset_falls_back_to_file() {
    let config = config_file();
    let store = store(&config).await;
    run_local(&store, set("notify", NOTIFY)).await.unwrap();

    let unset = SettingsAction::Unset {
        key: "notify".into(),
    };
    let outcome = run_local(&store, unset).await.unwrap();
    assert!(outcome.changed, "{}", outcome.output);
    assert_eq!(
        listed(&store, "notify").await,
        r#"notify = ["telegram-main:telegram:user:704338780"] (config file)"#
    );
}

/// A settings service over the database `config` names, as `serve` mounts
/// it, with the observer token set; returns its address.
async fn serve_settings(
    config: &Path,
) -> (String, Arc<SettingsStore>, tokio::task::JoinHandle<()>) {
    let (storage, settings) = open(config, schema()).await.unwrap();
    let tokens = ConnectorTokens::new(settings.current().derived.connector_tokens.clone());
    let service = settings_server(
        Arc::clone(&settings),
        storage,
        Arc::new(tokens),
        Some(OBSERVER_TOKEN.into()),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (format!("http://{address}"), settings, server)
}

#[tokio::test]
async fn settings_reload_calls_running_core() {
    let config = config_file();
    let (address, core, server) = serve_settings(&config).await;

    // A local change, made while the core runs, reaches it on reload.
    run_local(&*store(&config).await, set("notify", NOTIFY))
        .await
        .unwrap();
    let before = core.current().value("notify");
    assert_ne!(before, Some(serde_json::from_str(NOTIFY).unwrap()));

    let output = reload(address, Some(OBSERVER_TOKEN.into())).await.unwrap();
    assert_eq!(output, "reloaded: 1 overrides, 0 ignored");
    assert_eq!(
        core.current().value("notify"),
        Some(serde_json::from_str(NOTIFY).unwrap())
    );
    server.abort();
}

#[tokio::test]
async fn settings_reload_without_token_is_refused() {
    let config = config_file();
    let (address, _core, server) = serve_settings(&config).await;

    for token in [None, Some("wrong".to_owned())] {
        let error = reload(address.clone(), token.clone())
            .await
            .err()
            .unwrap_or_else(|| panic!("reload with {token:?} is refused"));
        assert!(
            format!("{error:#}").contains("valid bearer token required"),
            "{error:#}"
        );
    }
    server.abort();
}
