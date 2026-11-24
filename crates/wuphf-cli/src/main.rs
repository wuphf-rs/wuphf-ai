use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tonic::transport::Server;
use wuphf_core::Core;
use wuphf_functions::{FunctionHub, GrpcFunctionService, GrpcProviderService};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_server::ConnectorTransportServer;
use wuphf_proto::connector::function_transport_client::FunctionTransportClient;
use wuphf_proto::connector::function_transport_server::FunctionTransportServer;
use wuphf_proto::connector::health_transport_server::HealthTransportServer;
use wuphf_proto::connector::media_transport_server::MediaTransportServer;
use wuphf_proto::connector::observer_transport_server::ObserverTransportServer;
use wuphf_proto::connector::plugin_transport_server::PluginTransportServer;
use wuphf_proto::connector::provider_transport_server::ProviderTransportServer;
use wuphf_runtime::{
    Runtime,
    auth::{ConnectorTokens, any_token_auth},
    connector::ConnectorHub,
    connector_settings::ConnectorSettings,
    grpc::GrpcConnectorService,
    health::{GrpcHealthService, HealthMonitor, Wiring},
    live_sessions::LiveSessions,
    live_sessions::tool_from_wire,
    media::GrpcMediaService,
    observer_service::{GrpcObserverService, bearer_auth},
    plugin_settings::{GrpcPluginService, PluginSettings},
    settings_auth::settings_server,
};
use wuphf_settings::{LiveRouting, WuphfSchema};

mod backup;
mod health;
mod settings;
mod setup;
mod update;

#[derive(Parser)]
#[command(
    name = "wuphf-server",
    version,
    about = "Run the wuphf messaging replication runtime"
)]
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
    /// The open health problems (plan 079): read from the database, so it
    /// works while the core is down, or checked again by a running core with
    /// --run (WUPHF_OBSERVER_TOKEN). Exits 1 while a problem not ignored is
    /// open.
    Health {
        #[arg(long, default_value = "config/example.toml")]
        config: PathBuf,
        /// Ask the running core to probe and check now.
        #[arg(long)]
        run: bool,
        #[arg(long, default_value = "http://127.0.0.1:50051")]
        address: String,
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
    /// Back up the settings as one TOML file: the config file with every
    /// stored override applied, and every connector's stored settings.
    /// Without --with-secrets it names the token variables but holds no
    /// values. Works while the core is down.
    Export {
        #[arg(long, default_value = "config.local.toml")]
        config: PathBuf,
        /// Where the token variables' values come from (--with-secrets).
        #[arg(long, default_value = ".env.local")]
        env_file: PathBuf,
        /// Write the backup here (readable by its owner only) instead of
        /// printing it.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Include the values of the variables the options name.
        #[arg(long)]
        with_secrets: bool,
    },
    /// Restore a backup: the config file (the previous one kept as .bak), the
    /// env file's variables when the backup has them, and every stored
    /// override. Checked as the core would load it first; refused while a
    /// core runs.
    Import {
        /// The backup, as `export` writes it.
        file: PathBuf,
        #[arg(long, default_value = "config.local.toml")]
        config: PathBuf,
        #[arg(long, default_value = ".env.local")]
        env_file: PathBuf,
    },
    /// Update every installed wuphf binary (and those of the [updates]
    /// sources) to its newest release, checked against the release's signed
    /// checksums, then restart the units that run them. Run it as the user
    /// that owns the binaries (root, for /usr/local/bin).
    Update(update::UpdateArguments),
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
    let command = Arguments::parse().command;
    // The setup screen is the terminal: a log line would draw over it.
    if !matches!(command, Command::Setup { .. }) {
        init_logging();
    }
    match command {
        Command::Serve { config } => serve(config).await,
        Command::Health {
            config,
            run,
            address,
        } => {
            if health::run(&config, run, address).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Functions {
            command: FunctionsCommand::List { address },
        } => list_functions(address).await,
        Command::Settings { config, action } => settings::run(&config, action).await,
        Command::Setup { config, env_file } => setup::run(config, env_file),
        Command::Export {
            config,
            env_file,
            output,
            with_secrets,
        } => {
            let backup = backup::export_offline(&config, &env_file, with_secrets).await?;
            backup::write_backup(output.as_ref(), &backup)?;
            if let Some(output) = &output {
                eprintln!("wrote {}", output.display());
            }
            Ok(())
        }
        Command::Import {
            file,
            config,
            env_file,
        } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let imported = backup::import_offline(&config, &env_file, &text).await?;
            println!("{}", backup::imported_summary(&config, &imported));
            Ok(())
        }
        Command::Update(arguments) => update::run(arguments).await,
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
    // Held while the core runs, so an offline import never writes under it.
    let _database_lock = backup::lock_database(&backup::database_url(&path)?)
        .context("lock the core's database (is another core, or an import, using it?)")?;
    let (storage, settings) = settings::open(&path, WuphfSchema::new()).await?;
    // Connectors declaring their own settings get a section in the store
    // (stored declarations were restored by `open`).
    let connector_settings = Arc::new(ConnectorSettings::new(
        Arc::clone(&settings),
        storage.clone(),
    ));
    // Plugins (wuphf-misc) declare theirs through the plugin service.
    let plugin_settings = Arc::new(PluginSettings::new(Arc::clone(&settings), storage.clone()));
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
    // For the services behind the observer token that refuse everything when
    // it is unset (plugins, health).
    let observer_token_value = observer_token.clone();
    // Settings go through the core's settings service: connectors acting for
    // their owners, and the observer token as the owner.
    // The live connector sessions: the commands they declare, offered on their
    // sections in the settings panel, and the health probes (plan 079).
    let sessions = Arc::new(LiveSessions::default());
    let settings_service = settings_server(
        Arc::clone(&settings),
        Arc::clone(&storage),
        Arc::clone(&connector_tokens),
        observer_token.clone(),
        Arc::clone(&sessions),
    );
    let (runtime, channels, observer) = Runtime::channelled(
        Arc::new(Core::new(
            Arc::clone(&storage),
            LiveRouting::new(Arc::clone(&settings)),
        )),
        128,
    );
    // New releases are told to the owner through the notice targets.
    update::spawn_check(&configuration.updates, &path, runtime.owner_sender());
    // Plugins declare their settings with the observer token, which the
    // settings service treats as the owner; with none set, nothing is
    // accepted (as settings access is refused).
    let plugin_service = PluginTransportServer::with_interceptor(
        GrpcPluginService::new(plugin_settings),
        any_token_auth(
            observer_token
                .iter()
                .filter(|token| !token.is_empty())
                .cloned()
                .collect(),
        ),
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
    // The health checks (plan 079): what the stored problems pause is held
    // before anything is routed.
    let health = HealthMonitor::new(
        Arc::clone(&storage),
        Arc::clone(&sessions),
        runtime.owner_sender(),
        Arc::clone(&settings),
        Some(Arc::new(FunctionWiring(function_handle.clone()))),
    );
    health
        .restore()
        .await
        .context("restore the health pauses")?;
    let health_service = HealthTransportServer::with_interceptor(
        GrpcHealthService::new(Arc::clone(&health)),
        any_token_auth(
            observer_token_value
                .iter()
                .filter(|token| !token.is_empty())
                .cloned()
                .collect(),
        ),
    );
    let (hub, handle) = ConnectorHub::new(channels, connectors);
    let runtime_task = tokio::spawn(runtime.run());
    let health_task = health.spawn();
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
                .with_commands(sessions),
        ))
        .add_service(MediaTransportServer::with_interceptor(
            media_service,
            any_token_auth(media_tokens),
        ))
        .add_service(observer_service)
        .add_service(plugin_service)
        .add_service(health_service)
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
    health_task.abort();
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

/// The functions hub as the health checks read it: the providers' tools and
/// the functions no connected provider has offered for as long as a
/// connector may be away.
struct FunctionWiring(wuphf_functions::FunctionHubHandle);

impl Wiring for FunctionWiring {
    fn provider_tools(&self) -> BTreeMap<String, Vec<wuphf_model::ToolStatus>> {
        self.0
            .provider_tools()
            .into_iter()
            .map(|(provider, tools)| (provider, tools.into_iter().map(tool_from_wire).collect()))
            .collect()
    }

    fn unavailable_functions(&self) -> Vec<String> {
        let grace = u64::try_from(wuphf_health::OFFLINE_AFTER_MS).unwrap_or_default();
        self.0.unavailable_functions(Duration::from_millis(grace))
    }
}
