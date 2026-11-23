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
    connector_commands::ConnectorCommands,
    connector_settings::ConnectorSettings,
    grpc::GrpcConnectorService,
    media::GrpcMediaService,
    observer_service::{GrpcObserverService, bearer_auth},
    settings_auth::settings_server,
};
use wuphf_settings::{LiveRouting, WuphfSchema};

mod settings;
mod setup;

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
    /// View or change the runtime settings. Edits the database directly, so it
    /// works while the core is down.
    Settings {
        #[arg(long, default_value = "config/example.toml")]
        config: PathBuf,
        #[command(subcommand)]
        action: settings::SettingsSubcommand,
    },
    /// Edit the config file and the env file its options name (tokens can be
    /// generated) on a setup screen, every option with its description.
    /// Works while the core is down.
    Setup {
        #[arg(long, default_value = "config.local.toml")]
        config: PathBuf,
        #[arg(long, default_value = ".env.local")]
        env_file: PathBuf,
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
        Command::Settings { config, action } => settings::run(&config, action).await,
        Command::Setup { config, env_file } => setup::run(config, env_file),
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
    let (storage, settings) = settings::open(&path, WuphfSchema::new()).await?;
    // Connectors declaring their own settings get a section in the store
    // (stored declarations were restored by `open`).
    let connector_settings = Arc::new(ConnectorSettings::new(
        Arc::clone(&settings),
        storage.clone(),
    ));
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
    let observer_token = configuration
        .observer_token
        .as_ref()
        .map(|token| token.expose().to_owned());
    // Settings go through the core's settings service: connectors acting for
    // their owners, and the observer token as the owner.
    // The commands live connector sessions declare, offered on their sections
    // in the settings panel and run on their sessions.
    let commands = Arc::new(ConnectorCommands::default());
    let settings_service = settings_server(
        Arc::clone(&settings),
        Arc::clone(&storage),
        Arc::clone(&connector_tokens),
        observer_token.clone(),
        Arc::clone(&commands),
    );
    let (runtime, channels, observer) = Runtime::channelled(
        Arc::new(Core::new(storage, LiveRouting::new(Arc::clone(&settings)))),
        128,
    );
    let observer_service = ObserverTransportServer::with_interceptor(
        GrpcObserverService::new(observer).with_sender(runtime.owner_sender()),
        bearer_auth(observer_token),
    );
    if configuration.observer_token.is_none() {
        tracing::warn!("observer service has no token set; bind it to a local address only");
        tracing::warn!(
            "settings service: no observer token set; only connectors acting for their owners can change settings"
        );
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
        .add_service(ConnectorTransportServer::new(
            GrpcConnectorService::new(handle, connector_tokens)
                .with_settings(connector_settings)
                .with_commands(commands),
        ))
        .add_service(MediaTransportServer::with_interceptor(
            media_service,
            any_token_auth(media_tokens),
        ))
        .add_service(observer_service)
        // No interceptor: the service's authorizer checks the token itself,
        // since it must know which connector presented it.
        .add_service(settings_service)
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
