use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tonic::transport::Server;
use wuphf_core::{ConnectorInfo, Core, RoutingConfig};
use wuphf_model::{
    ConnectorCapabilities, ConnectorId, ConnectorRegistration, DeliveryOptions, Platform, Rule,
};
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_runtime::{
    Runtime, connector::ConnectorHub, grpc::GrpcConnectorService, media::GrpcMediaService,
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
    #[serde(default)]
    connectors: Vec<ConnectorConfiguration>,
    #[serde(default)]
    rules: Vec<Rule>,
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
    #[serde(default)]
    delivery: DeliveryOptions,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Arguments::parse().command {
        Command::Serve { config } => serve(config).await,
    }
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
                delivery: connector.delivery,
            },
        );
        allowed.push(ConnectorRegistration {
            id,
            platform,
            capabilities,
        });
    }
    let config = Arc::new(RoutingConfig::new(configuration.rules, connector_infos));

    let media_service = GrpcMediaService::new(
        Arc::clone(&storage),
        configuration.max_media_bytes,
        Duration::from_secs(configuration.orphan_media_retention_seconds),
    );
    let (runtime, channels) = Runtime::channelled(Arc::new(Core::new(storage, config)), 128);
    let (hub, handle) = ConnectorHub::new(channels, allowed);
    let runtime_task = tokio::spawn(runtime.run());
    let hub_task = tokio::spawn(hub.run());
    println!(
        "wuphf connector gRPC server listening on {}",
        configuration.listen
    );
    let server = Server::builder()
        .add_service(ConnectorTransportServer::new(GrpcConnectorService::new(
            handle,
        )))
        .add_service(MediaTransportServer::new(media_service))
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
        _ if name.starts_with("other:") => Ok(Platform::Other(name[6..].to_owned())),
        _ => bail!("unsupported platform {name}"),
    }
}
