//! `wuphf` core server executable entry point.

use anyhow::Context;
use clap::Parser;
use std::{net::SocketAddr, sync::Arc};
use tonic::transport::Server;
use tracing_subscriber::EnvFilter;
use wuphf_core::{Config, ConnectorService, Engine, Store};
use wuphf_proto::v1::connector_server::ConnectorServer;

#[derive(Parser)]
/// Command-line options for starting core service.
struct Args {
    /// Path to TOML runtime topology and delivery configuration.
    #[arg(short, long, default_value = "config/example.toml")]
    config: String,
}

#[tokio::main]
/// Loads configuration and serves authenticated connector streams.
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let config = Arc::new(Config::load(&args.config)?);
    let listen: SocketAddr = config
        .server
        .listen
        .parse()
        .with_context(|| format!("invalid listen address '{}'", config.server.listen))?;
    let store = Store::open(&config).await?;
    let service = ConnectorService::new(Engine::new(config, store));
    tracing::info!(%listen, "starting wuphf core");
    Server::builder()
        .add_service(ConnectorServer::new(service))
        .serve(listen)
        .await?;
    Ok(())
}
