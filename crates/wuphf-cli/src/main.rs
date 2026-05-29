use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tonic::transport::Server;
use uuid::Uuid;
use wuphf_core::Core;
use wuphf_model::{
    ConnectorId, ConnectorRegistration, DispatchPayload, Endpoint, EndpointId, EventPayload,
    GroupId, InboundMessage, NormalizedEvent, Platform, ReplicationGroup,
};
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_runtime::{
    Runtime, connector::ConnectorHub, grpc::GrpcConnectorService, media::GrpcMediaService,
};
use wuphf_storage::{SqliteStorage, Storage};

#[derive(Parser)]
#[command(about = "Run the wuphf messaging replication runtime")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Demo {
        #[arg(long, default_value = "hello from WhatsApp")]
        text: String,
        #[arg(long, default_value = "sqlite::memory:")]
        database_url: String,
    },
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
    endpoints: Vec<EndpointConfiguration>,
    groups: Vec<GroupConfiguration>,
    connectors: Vec<ConnectorConfiguration>,
}

fn default_max_media_bytes() -> usize {
    32 * 1024 * 1024
}

fn default_orphan_media_retention_seconds() -> u64 {
    24 * 60 * 60
}

#[derive(Deserialize)]
struct EndpointConfiguration {
    id: Uuid,
    platform: String,
    chat_id: String,
    thread_id: Option<String>,
}

#[derive(Deserialize)]
struct GroupConfiguration {
    id: Uuid,
    endpoints: Vec<Uuid>,
}

#[derive(Deserialize)]
struct ConnectorConfiguration {
    id: String,
    platform: String,
    endpoints: Vec<Uuid>,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Arguments::parse().command {
        Command::Demo { text, database_url } => demo(text, &database_url).await,
        Command::Serve { config } => serve(config).await,
    }
}

async fn demo(text: String, database_url: &str) -> Result<()> {
    let storage = Arc::new(SqliteStorage::connect(database_url).await?);
    let whatsapp = EndpointId::new();
    let telegram = EndpointId::new();
    let group_id = GroupId::new();

    storage
        .put_endpoint(Endpoint {
            id: whatsapp,
            platform: Platform::WhatsApp,
            chat_id: "demo@s.whatsapp.net".into(),
            thread_id: None,
        })
        .await?;
    storage
        .put_endpoint(Endpoint {
            id: telegram,
            platform: Platform::Telegram,
            chat_id: "-100-demo".into(),
            thread_id: Some("topic-1".into()),
        })
        .await?;
    storage
        .put_group(ReplicationGroup {
            id: group_id,
            endpoints: vec![whatsapp, telegram],
        })
        .await?;

    let (runtime, mut channels) = Runtime::channelled(Arc::new(Core::new(storage)), 8);
    let runtime_task = tokio::spawn(runtime.run());
    channels
        .events
        .send(NormalizedEvent {
            origin_endpoint: whatsapp,
            group_id,
            payload: EventPayload::MessageCreate(InboundMessage {
                platform_message_id: "wa-demo-1".into(),
                sender: "demo-user".into(),
                participant: None,
                text: Some(text),
                attachments: vec![],
                reply_to_platform_message_id: None,
                timestamp_ms: 0,
            }),
        })
        .await
        .context("event bus closed")?;

    let dispatch = channels
        .dispatches
        .recv()
        .await
        .context("demonstration event produced no dispatch")?;
    let target_endpoint = dispatch.command.target_endpoint;
    let DispatchPayload::MessageCreate { message, .. } = dispatch.command.payload;
    println!(
        "logical message {} dispatched to Telegram endpoint {}: {}",
        message.id,
        target_endpoint,
        message.text.as_deref().unwrap_or_default()
    );

    drop(channels.events);
    runtime_task.await??;
    Ok(())
}

async fn serve(path: PathBuf) -> Result<()> {
    let source = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("failed to read configuration {}", path.display()))?;
    let configuration: Configuration =
        toml::from_str(&source).context("failed to parse TOML configuration")?;
    let storage = Arc::new(SqliteStorage::connect(&configuration.database_url).await?);
    for endpoint in &configuration.endpoints {
        storage
            .put_endpoint(Endpoint {
                id: EndpointId(endpoint.id),
                platform: platform(&endpoint.platform)?,
                chat_id: endpoint.chat_id.clone(),
                thread_id: endpoint.thread_id.clone(),
            })
            .await?;
    }
    for group in &configuration.groups {
        storage
            .put_group(ReplicationGroup {
                id: GroupId(group.id),
                endpoints: group.endpoints.iter().copied().map(EndpointId).collect(),
            })
            .await?;
    }
    let allowed = configuration
        .connectors
        .iter()
        .map(|connector| {
            Ok(ConnectorRegistration {
                id: ConnectorId(connector.id.clone()),
                platform: platform(&connector.platform)?,
                endpoints: connector
                    .endpoints
                    .iter()
                    .copied()
                    .map(EndpointId)
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let media_service = GrpcMediaService::new(
        Arc::clone(&storage),
        configuration.max_media_bytes,
        Duration::from_secs(configuration.orphan_media_retention_seconds),
    );
    let (runtime, channels) = Runtime::channelled(Arc::new(Core::new(storage)), 128);
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
