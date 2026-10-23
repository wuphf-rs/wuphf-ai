use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tonic::transport::Server;
use wuphf_core::{Core, RoutingConfig};
use wuphf_model::{
    Connector, ConnectorCapabilities, ConnectorId, ConnectorInfo, NoticeTarget, Platform, Rule,
};
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_proto::connector::observer_transport_server::ObserverTransportServer;
use wuphf_runtime::{
    Runtime,
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
    notify: Vec<NoticeTarget>,
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

    let mut connector_infos = HashMap::new();
    let mut allowed = Vec::new();
    for connector in &configuration.connectors {
        let id = ConnectorId(connector.id.clone());
        let platform = platform(&connector.platform)?;
        let capabilities = ConnectorCapabilities {
            supports_threads: connector.supports_threads,
            supports_topic_create: connector.supports_topic_create,
        };
        connector_infos.insert(
            id.clone(),
            ConnectorInfo {
                platform: platform.clone(),
                capabilities,
            },
        );
        allowed.push(Connector {
            id,
            platform,
            capabilities,
        });
    }
    let config = Arc::new(
        RoutingConfig::new(configuration.rules, connector_infos)
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
    let (hub, handle) = ConnectorHub::new(channels, allowed);
    let runtime_task = tokio::spawn(runtime.run());
    let hub_task = tokio::spawn(hub.run());
    tracing::info!(listen = %configuration.listen, "wuphf connector gRPC server listening");
    let server = Server::builder()
        .add_service(ConnectorTransportServer::new(GrpcConnectorService::new(
            handle,
        )))
        .add_service(MediaTransportServer::new(media_service))
        .add_service(observer_service)
        .serve(configuration.listen);
    tokio::select! {
        result = server => result?,
        result = runtime_task => result??,
    }
    hub_task.abort();
    Ok(())
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
}
