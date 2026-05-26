//! `wuphf` core server executable entry point.

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, sync::Arc};
use tonic::transport::Server;
use tracing_subscriber::EnvFilter;
use wuphf_core::{
    admin::{AdminService, DispatchFilter},
    Config, ConnectorService, Engine, Store,
};
use wuphf_proto::v1::connector_server::ConnectorServer;

#[derive(Parser)]
/// Command-line options for starting core service.
struct Args {
    /// Path to TOML runtime topology and delivery configuration.
    #[arg(short, long, default_value = "config/example.toml")]
    config: String,
    /// Optional operator command; absent command runs gRPC server.
    #[command(subcommand)]
    command: Option<Command>,
}

/// Operator commands over durable core state.
#[derive(Subcommand)]
enum Command {
    /// Inspects or recovers durable dispatch commands.
    Dispatches {
        /// Dispatch operator action.
        #[command(subcommand)]
        action: DispatchAction,
    },
}

/// Supported durable dispatch administration actions.
#[derive(Subcommand)]
enum DispatchAction {
    /// Lists dispatch commands and current delivery state.
    List {
        /// Shows only terminal failures requiring operator action.
        #[arg(long)]
        terminal_only: bool,
        /// Filters records to one connector identity.
        #[arg(long)]
        connector: Option<String>,
        /// Filters records to one endpoint identity.
        #[arg(long)]
        endpoint: Option<String>,
        /// Limits returned rows, clamped to 1000.
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Reopens one terminal failed dispatch for delivery.
    Retry {
        /// Durable dispatch command identity to retry.
        dispatch_id: String,
    },
}

#[tokio::main]
/// Loads configuration and serves authenticated connector streams.
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let config = Arc::new(Config::load(&args.config)?);
    let store = Store::open(&config).await?;
    if let Some(command) = args.command {
        return run_command(command, store).await;
    }
    let listen: SocketAddr = config
        .server
        .listen
        .parse()
        .with_context(|| format!("invalid listen address '{}'", config.server.listen))?;
    let service = ConnectorService::new(Engine::new(config, store));
    tracing::info!(%listen, "starting wuphf core");
    Server::builder()
        .add_service(ConnectorServer::new(service))
        .serve_with_shutdown(listen, shutdown_signal())
        .await?;
    Ok(())
}

/// Waits for process termination signal so Tonic can drain active connections.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler must install");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::error!(%error, "failed to listen for Ctrl-C");
                }
            }
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to listen for Ctrl-C");
    }

    tracing::info!("shutdown signal received");
}

/// Executes one operator command against durable state and exits.
async fn run_command(command: Command, store: Store) -> anyhow::Result<()> {
    let admin = AdminService::new(store);
    match command {
        Command::Dispatches {
            action:
                DispatchAction::List {
                    terminal_only,
                    connector,
                    endpoint,
                    limit,
                },
        } => {
            let records = admin
                .list_dispatches(&DispatchFilter {
                    connector_id: connector,
                    endpoint_id: endpoint,
                    terminal_only,
                    limit,
                })
                .await?;
            println!(
                "{:<36} {:<16} {:<16} {:<10} {:<8} {:<8} {:<14} ERROR",
                "DISPATCH",
                "ENDPOINT",
                "CONNECTOR",
                "STATUS",
                "ATTEMPTS",
                "TERMINAL",
                "NEXT_ATTEMPT"
            );
            for record in records {
                println!(
                    "{:<36} {:<16} {:<16} {:<10} {:<8} {:<8} {:<14} {}",
                    record.id,
                    record.target_endpoint_id,
                    record.target_connector_id,
                    record.status,
                    record.attempt_count,
                    record.terminal_failure,
                    record
                        .next_attempt_at_unix_ms
                        .map_or_else(|| "-".to_owned(), |timestamp| timestamp.to_string()),
                    record.last_error.as_deref().unwrap_or("-")
                );
            }
        }
        Command::Dispatches {
            action: DispatchAction::Retry { dispatch_id },
        } => {
            admin.retry_dispatch(&dispatch_id).await?;
            println!("dispatch {dispatch_id} reopened; delivery resumes on connector activity");
        }
    }
    Ok(())
}
