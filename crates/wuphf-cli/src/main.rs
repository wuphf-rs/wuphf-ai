use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tonic::transport::Server;
use wuphf_core::{Core, RoutingConfig};
use wuphf_functions::{FunctionHub, FunctionsConfig, GrpcFunctionService, GrpcProviderService};
use wuphf_model::{ChatRef, Connector, ConnectorCapabilities, ConnectorId, Platform, Rule};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_proto::connector::function_transport_client::FunctionTransportClient;
use wuphf_proto::connector::function_transport_server::FunctionTransportServer;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_proto::connector::observer_transport_server::ObserverTransportServer;
use wuphf_proto::connector::provider_transport_server::ProviderTransportServer;
use wuphf_runtime::{
    Runtime,
    auth::{ConnectorTokens, any_token_auth},
    connector::ConnectorHub,
    grpc::GrpcConnectorService,
    media::GrpcMediaService,
    observer_service::{GrpcObserverService, bearer_auth},
};
use wuphf_storage::SqliteStorage;

#[derive(Parser)]
#[command(about = "Run the wuphf messaging replication runtime")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, default_value = "config/example.toml")]
        config: PathBuf,
    },
    /// Inspect the remote functions registered with a running core.
    Functions {
        #[command(subcommand)]
        command: FunctionsCommand,
    },
}

#[derive(Subcommand)]
enum FunctionsCommand {
    /// List functions with their provider, load and counters. Reads the
    /// invoke token from WUPHF_INVOKE_TOKEN when set.
    List {
        #[arg(long, default_value = "http://127.0.0.1:50051")]
        address: String,
    },
}

#[derive(Deserialize)]
struct Configuration {
    listen: SocketAddr,
    database_url: String,
    #[serde(default = "default_max_media_bytes")]
    max_media_bytes: usize,
    #[serde(default = "default_orphan_media_retention_seconds")]
    orphan_media_retention_seconds: u64,
    /// Bearer token required by the observer service (the TUI read surface). When
    /// unset the observer is unauthenticated — only safe on a local-only bind.
    #[serde(default)]
    observer_token: Option<String>,
    #[serde(default)]
    connectors: Vec<ConnectorConfiguration>,
    #[serde(default)]
    rules: Vec<Rule>,
    /// Chats that receive operational notices from connectors (e.g. a WhatsApp
    /// session ending). A notice is never sent through the connector that
    /// raised it, so listing targets on more than one connector is what keeps
    /// the owner reachable when one platform is down.
    #[serde(default)]
    notify: Vec<ChatRef>,
    /// Remote functions: which provider processes may serve which function
    /// names, and the deadlines and limits calls run under.
    #[serde(default)]
    functions: FunctionsConfig,
}

fn default_max_media_bytes() -> usize {
    32 * 1024 * 1024
}

fn default_orphan_media_retention_seconds() -> u64 {
    24 * 60 * 60
}

#[derive(Deserialize)]
struct ConnectorConfiguration {
    id: String,
    platform: String,
    /// Environment variable holding this connector's token. The connector
    /// presents it as `authorization: Bearer <token>`; required.
    token_env: String,
    #[serde(default)]
    supports_threads: bool,
    #[serde(default)]
    supports_topic_create: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    match Arguments::parse().command {
        Command::Serve { config } => serve(config).await,
        Command::Functions {
            command: FunctionsCommand::List { address },
        } => list_functions(address).await,
    }
}

/// Install the global tracing subscriber. Verbosity is controlled with the
/// standard `RUST_LOG` filter (e.g. `RUST_LOG=wuphf_runtime=debug,info`),
/// defaulting to `info` for everything.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

async fn serve(path: PathBuf) -> Result<()> {
    let source = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("failed to read configuration {}", path.display()))?;
    let configuration: Configuration =
        toml::from_str(&source).context("failed to parse TOML configuration")?;
    let storage = Arc::new(SqliteStorage::connect(&configuration.database_url).await?);

    let mut connectors = Vec::new();
    for connector in &configuration.connectors {
        connectors.push(Connector {
            id: ConnectorId(connector.id.clone()),
            platform: platform(&connector.platform)?,
            capabilities: ConnectorCapabilities {
                supports_threads: connector.supports_threads,
                supports_topic_create: connector.supports_topic_create,
            },
        });
    }
    let connector_tokens = Arc::new(connector_tokens(&configuration.connectors, |name| {
        std::env::var(name).ok()
    })?);
    let config = Arc::new(
        RoutingConfig::new(configuration.rules, connectors.clone())
            .with_notice_targets(configuration.notify),
    );

    let media_service = GrpcMediaService::new(
        Arc::clone(&storage),
        configuration.max_media_bytes,
        Duration::from_secs(configuration.orphan_media_retention_seconds),
    );
    let (runtime, channels, observer) =
        Runtime::channelled(Arc::new(Core::new(storage, config)), 128);
    let observer_service = ObserverTransportServer::with_interceptor(
        GrpcObserverService::new(observer).with_sender(runtime.owner_sender()),
        bearer_auth(configuration.observer_token.clone()),
    );
    if configuration.observer_token.is_none() {
        tracing::warn!("observer service has no token set; bind it to a local address only");
    }
    let functions = Arc::new(
        configuration
            .functions
            .resolve(|name| std::env::var(name).ok())
            .context("invalid [functions] configuration")?,
    );
    if functions.invoke_token().is_none() {
        tracing::warn!("function service has no invoke token set; bind it to a local address only");
    }
    // Media is used by every connector and by providers (mediaconv converts
    // stickers through it), so any of their tokens is accepted.
    let media_tokens = connector_tokens
        .tokens()
        .chain(functions.provider_tokens())
        .map(str::to_owned)
        .collect();
    let (function_hub, function_handle) = FunctionHub::new(Arc::clone(&functions));
    let (hub, handle) = ConnectorHub::new(channels, connectors);
    let runtime_task = tokio::spawn(runtime.run());
    let hub_task = tokio::spawn(hub.run());
    let function_task = tokio::spawn(function_hub.run());
    tracing::info!(listen = %configuration.listen, "wuphf connector gRPC server listening");
    let server = Server::builder()
        // Server-wide: also lets the core notice dead connector connections.
        .http2_keepalive_interval(Some(functions.keepalive_interval()))
        .http2_keepalive_timeout(Some(functions.keepalive_timeout()))
        .add_service(ConnectorTransportServer::new(GrpcConnectorService::new(
            handle,
            connector_tokens,
        )))
        .add_service(MediaTransportServer::with_interceptor(
            media_service,
            any_token_auth(media_tokens),
        ))
        .add_service(observer_service)
        .add_service(FunctionTransportServer::with_interceptor(
            GrpcFunctionService::new(function_handle.clone(), Arc::clone(&functions)),
            bearer_auth(functions.invoke_token().map(str::to_owned)),
        ))
        .add_service(ProviderTransportServer::new(GrpcProviderService::new(
            function_handle,
            Arc::clone(&functions),
        )))
        .serve(configuration.listen);
    tokio::select! {
        result = server => result?,
        result = runtime_task => result??,
    }
    hub_task.abort();
    function_task.abort();
    Ok(())
}

/// Print every function a running core knows of, one line each.
async fn list_functions(address: String) -> Result<()> {
    let mut client = FunctionTransportClient::connect(address.clone())
        .await
        .with_context(|| format!("failed to connect to {address}"))?;
    let mut request = tonic::Request::new(pb::ListFunctionsRequest {});
    // The token comes from the environment, never a flag, so it stays out of `ps`.
    if let Ok(token) = std::env::var("WUPHF_INVOKE_TOKEN") {
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .context("WUPHF_INVOKE_TOKEN is not a valid header value")?,
        );
    }
    let functions = client
        .list_functions(request)
        .await
        .context("ListFunctions failed")?
        .into_inner()
        .functions;
    if functions.is_empty() {
        println!("no functions registered, queued or called yet");
    }
    for status in functions {
        let function = status.function.unwrap_or_default();
        let provider = if status.provider_id.is_empty() {
            "-"
        } else {
            &status.provider_id
        };
        println!(
            "{}@{}  provider={provider}  in_flight={}/{}  queued={}  completed={}  failed={}  abandoned={}",
            function.name,
            function.major,
            status.in_flight,
            status.max_concurrency,
            status.queued,
            status.completed,
            status.failed,
            status.abandoned,
        );
    }
    Ok(())
}

/// Read every connector's token from `env`, failing startup when one is unset
/// or empty, as provider tokens do.
fn connector_tokens(
    connectors: &[ConnectorConfiguration],
    env: impl Fn(&str) -> Option<String>,
) -> Result<ConnectorTokens> {
    let mut tokens = HashMap::new();
    for connector in connectors {
        let Some(token) = env(&connector.token_env).filter(|token| !token.is_empty()) else {
            bail!(
                "connector {} reads its token from {}, which is unset or empty",
                connector.id,
                connector.token_env
            );
        };
        tokens.insert(ConnectorId(connector.id.clone()), token);
    }
    Ok(ConnectorTokens::new(tokens))
}

fn platform(name: &str) -> Result<Platform> {
    match name {
        "whatsapp" => Ok(Platform::WhatsApp),
        "telegram" => Ok(Platform::Telegram),
        "discord" => Ok(Platform::Discord),
        "signal" => Ok(Platform::Signal),
        "slack" => Ok(Platform::Slack),
        "matrix" => Ok(Platform::Matrix),
        "instagram" => Ok(Platform::Instagram),
        _ if name.starts_with("other:") => Ok(Platform::Other(name[6..].to_owned())),
        _ => bail!("unsupported platform {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{ChatFilter, RuleKind};

    /// The shipped example config is the reference for the rule format; it must
    /// always parse, and its flows must land in the expected variants.
    #[test]
    fn example_configuration_parses() {
        let configuration: Configuration =
            toml::from_str(include_str!("../../../config/example.toml")).unwrap();
        assert_eq!(configuration.rules.len(), 2);
        for rule in &configuration.rules {
            let RuleKind::ManyToOne {
                source_chats_filter,
                destination_chat,
                ..
            } = &rule.flow
            else {
                panic!("example rules are many-to-one mirrors");
            };
            assert_eq!(*source_chats_filter, ChatFilter::All);
            assert_eq!(destination_chat.chat_id, "telegram:channel:2060340733");
        }
        assert_eq!(configuration.notify.len(), 1);
        assert_eq!(
            configuration.notify[0].connector_id,
            ConnectorId("telegram-main".into())
        );
        assert_eq!(
            configuration.notify[0].address.chat_id,
            "telegram:user:704338780"
        );
        assert_eq!(configuration.notify[0].address.thread_id, None);
    }

    #[test]
    fn one_to_one_rule_and_filters_parse() {
        let configuration: Configuration = toml::from_str(
            r#"
            listen = "127.0.0.1:50051"
            database_url = "sqlite://test.sqlite"

            [[rules]]
            id = "direct"
            [rules.flow]
            kind = "one-to-one"
            source_connector = "whatsapp-main"
            source_chat = { chat_id = "123@s.whatsapp.net" }
            destination_connector = "telegram-main"
            destination_chat = { chat_id = "-100123", thread_id = "12" }

            [[rules]]
            id = "filtered-mirror"
            [rules.flow]
            kind = "many-to-one"
            source_connector = "whatsapp-main"
            destination_connector = "telegram-main"
            destination_chat = { chat_id = "-100999" }
            [rules.flow.source_chats_filter]
            kind = "not"
            chats = [{ chat_id = "spam@g.us" }]
            "#,
        )
        .unwrap();
        let RuleKind::OneToOne {
            source_chat,
            destination_chat,
            ..
        } = &configuration.rules[0].flow
        else {
            panic!("first rule is one-to-one");
        };
        assert_eq!(source_chat.chat_id, "123@s.whatsapp.net");
        assert_eq!(destination_chat.thread_id.as_deref(), Some("12"));
        let RuleKind::ManyToOne {
            source_chats_filter: ChatFilter::Not { chats },
            ..
        } = &configuration.rules[1].flow
        else {
            panic!("second rule is a filtered mirror");
        };
        assert_eq!(chats[0].chat_id, "spam@g.us");
    }

    #[test]
    fn connector_without_token_env_is_rejected() {
        let error = toml::from_str::<Configuration>(
            r#"
            listen = "127.0.0.1:50051"
            database_url = "sqlite://test.sqlite"

            [[connectors]]
            id = "telegram-main"
            platform = "telegram"
            "#,
        )
        .err()
        .expect("a connector needs token_env");
        assert!(error.to_string().contains("token_env"), "{error}");
    }

    #[test]
    fn connector_tokens_are_read_from_the_environment() {
        let configuration: Configuration =
            toml::from_str(include_str!("../../../config/example.toml")).unwrap();
        let tokens = connector_tokens(&configuration.connectors, |name| {
            Some(format!("secret-{name}"))
        })
        .unwrap();
        assert!(tokens.authenticates(
            &ConnectorId("telegram-main".into()),
            Some("secret-WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN"),
        ));

        let error = connector_tokens(&configuration.connectors, |name| {
            (name != "WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN").then(|| "x".to_owned())
        })
        .expect_err("an unset token fails startup");
        assert_eq!(
            error.to_string(),
            "connector telegram-main reads its token from WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN, which is unset or empty"
        );
        assert!(
            connector_tokens(&configuration.connectors, |_| Some(String::new())).is_err(),
            "an empty token fails startup"
        );
    }

    #[test]
    fn functions_section_parses() {
        let configuration: Configuration = toml::from_str(
            r#"
            listen = "127.0.0.1:50051"
            database_url = "sqlite://test.sqlite"

            [functions]
            default_deadline_ms = 30000
            max_deadline_ms = 60000
            invoke_token_env = "WUPHF_INVOKE_TOKEN"

            [[functions.providers]]
            id = "mediaconv"
            token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
            functions = ["media.convert"]

            [functions.overrides."media.convert"]
            default_deadline_ms = 20000
            "#,
        )
        .unwrap();
        let functions = &configuration.functions;
        assert_eq!(functions.providers.len(), 1);
        assert_eq!(functions.providers[0].id, "mediaconv");
        assert_eq!(
            functions.overrides["media.convert"].default_deadline_ms,
            Some(20_000)
        );
        // Unset fields keep their defaults.
        assert_eq!(functions.max_queued_calls, 256);
    }
}
