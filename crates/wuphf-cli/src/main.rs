use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tonic::transport::Server;
use wuphf_core::Core;
use wuphf_functions::{FunctionHub, GrpcFunctionService, GrpcProviderService};
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
use wuphf_settings::{Configuration, LiveRouting, SettingsStorage, WuphfSchema};
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
    let base = wuphf_settings::base(&path)
        .with_context(|| format!("failed to load configuration {}", path.display()))?;
    // The database holds the runtime overrides, so its URL comes from the file
    // and the environment only; it can never be a runtime key.
    let file: Configuration = base.extract().context("invalid configuration")?;
    let storage = Arc::new(SqliteStorage::connect(&file.database_url).await?);
    let settings = Arc::new(
        wuphf_settings::load(
            WuphfSchema::new(),
            base,
            SettingsStorage::new(Arc::clone(&storage)),
        )
        .await
        .context("invalid configuration")?,
    );
    let snapshot = settings.current();
    let configuration = &snapshot.config;
    let derived = &snapshot.derived;
    let connectors = derived.connectors.clone();
    let connector_tokens = Arc::new(ConnectorTokens::new(derived.connector_tokens.clone()));

    let media_service = GrpcMediaService::new(
        Arc::clone(&storage),
        configuration.max_media_bytes,
        Duration::from_secs(configuration.orphan_media_retention_seconds),
    );
    let (runtime, channels, observer) = Runtime::channelled(
        Arc::new(Core::new(storage, LiveRouting::new(Arc::clone(&settings)))),
        128,
    );
    let observer_service = ObserverTransportServer::with_interceptor(
        GrpcObserverService::new(observer).with_sender(runtime.owner_sender()),
        bearer_auth(
            configuration
                .observer_token
                .as_ref()
                .map(|token| token.expose().to_owned()),
        ),
    );
    if configuration.observer_token.is_none() {
        tracing::warn!("observer service has no token set; bind it to a local address only");
    }
    let functions = Arc::clone(&derived.functions);
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
