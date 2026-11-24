//! `wuphf settings`: view and change the runtime settings from a terminal.
//!
//! The local actions open the same settings store the core uses, over the
//! core's database, so every change is validated exactly as at runtime and
//! the command works while the core is down. `reload` asks a running core to
//! re-read the config file and the stored settings through its settings
//! service.

use anyhow::{Context, Result};
use botconf::cli::SettingsAction;
use botconf_grpc::proto::{
    ExportRequest, ImportRequest, ReloadRequest, settings_client::SettingsClient,
};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wuphf_runtime::connector_settings::ConnectorSettings;
use wuphf_runtime::plugin_settings::PluginSettings;
use wuphf_settings::{Configuration, SettingsStorage, SettingsStore, WuphfSchema};
use wuphf_storage::SqliteStorage;

/// Printed after a local change: a running core holds its own copy of the
/// settings until it reloads them.
pub(crate) const RELOAD_NOTE: &str =
    "note: a running core applies this after \"wuphf settings reload\" or a restart";

#[derive(Subcommand)]
pub(crate) enum SettingsSubcommand {
    #[command(flatten)]
    Local(SettingsAction),
    /// Ask a running core to re-read the config file, the environment and the
    /// stored settings. Reads the token from WUPHF_OBSERVER_TOKEN.
    Reload {
        #[arg(long, default_value = "http://127.0.0.1:50051")]
        address: String,
    },
    /// A backup of a running core's settings, never with secrets (see
    /// `wuphf-server export`). Reads the token from WUPHF_OBSERVER_TOKEN.
    Export {
        #[arg(long, default_value = "http://127.0.0.1:50051")]
        address: String,
        /// Write the backup here instead of printing it.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Restore into a running core what of a backup can change at runtime;
    /// lists what needs `wuphf-server import` (offline). Reads the token from
    /// WUPHF_OBSERVER_TOKEN.
    Import {
        /// The backup, as `export` writes it.
        file: PathBuf,
        #[arg(long, default_value = "http://127.0.0.1:50051")]
        address: String,
        /// Only check it: change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// The core's database and the settings store over it, built from the config
/// file and the environment exactly as `serve` builds them, without starting
/// any server. Connectors' and plugins' stored settings declarations are
/// restored, so their sections can be edited while they are down.
pub(crate) async fn open(
    path: &Path,
    schema: WuphfSchema,
) -> Result<(Arc<SqliteStorage>, Arc<SettingsStore>)> {
    let base = wuphf_settings::base(path)
        .with_context(|| format!("failed to load configuration {}", path.display()))?;
    // The database holds the runtime overrides, so its URL comes from the file
    // and the environment only; it can never be a runtime key.
    let file: Configuration = base.extract().context("invalid configuration")?;
    let storage = Arc::new(SqliteStorage::connect(&file.database_url).await?);
    // The store re-reads the file and the environment on every reload, so an
    // edited config file applies without a restart.
    let source = wuphf_settings::source(path.to_path_buf());
    let settings = wuphf_settings::load(schema, source, SettingsStorage::new(Arc::clone(&storage)))
        .await
        .context("invalid configuration")?;
    let settings = Arc::new(settings);
    ConnectorSettings::new(Arc::clone(&settings), storage.clone())
        .restore()
        .await
        .context("failed to restore connector settings declarations")?;
    PluginSettings::new(Arc::clone(&settings), storage.clone())
        .restore()
        .await
        .context("failed to restore plugin settings declarations")?;
    Ok((storage, settings))
}

/// What a local action printed, and whether it changed a setting.
pub(crate) struct LocalOutcome {
    pub(crate) output: String,
    pub(crate) changed: bool,
}

/// Runs a local action against `store`.
pub(crate) async fn run_local(
    store: &SettingsStore,
    action: SettingsAction,
) -> Result<LocalOutcome> {
    let outcome = botconf::command::execute(store, action.into(), None).await?;
    Ok(LocalOutcome {
        output: botconf::cli::render(&outcome),
        changed: outcome.change().is_some(),
    })
}

/// The settings service at `address`.
async fn connect(address: &str) -> Result<SettingsClient<tonic::transport::Channel>> {
    SettingsClient::connect(address.to_owned())
        .await
        .with_context(|| format!("failed to connect to {address}"))
}

/// `message` as a request of the observer, when `token` is set.
fn request<T>(message: T, token: Option<&str>) -> Result<tonic::Request<T>> {
    let mut request = tonic::Request::new(message);
    if let Some(token) = token {
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .context("WUPHF_OBSERVER_TOKEN is not a valid header value")?,
        );
    }
    Ok(request)
}

/// A backup of the core at `address`'s settings.
pub(crate) async fn export_online(address: &str, token: Option<&str>) -> Result<String> {
    let response = connect(address)
        .await?
        .export(request(ExportRequest { actor: None }, token)?)
        .await
        .map_err(|status| anyhow::anyhow!("Export failed: {}", status.message()))?;
    Ok(response.into_inner().backup)
}

/// Restores into the core at `address` what of `backup` can change at
/// runtime, reporting what it set and what needs the offline import.
pub(crate) async fn import_online(
    address: &str,
    token: Option<&str>,
    backup: String,
    dry_run: bool,
) -> Result<String> {
    let response = connect(address)
        .await?
        .import(request(
            ImportRequest {
                actor: None,
                backup,
                dry_run,
            },
            token,
        )?)
        .await
        .map_err(|status| anyhow::anyhow!("Import failed: {}", status.message()))?
        .into_inner();
    let mut lines = Vec::new();
    let verb = if dry_run { "would set" } else { "set" };
    match response.applied.as_slice() {
        [] => lines.push("nothing to set: the runtime settings match the backup".to_owned()),
        keys => lines.push(format!("{verb}: {}", keys.join(", "))),
    }
    for refused in &response.rejected {
        lines.push(format!("refused {}: {}", refused.key, refused.reason));
    }
    if !response.offline.is_empty() {
        lines.push(format!(
            "differ, but only `wuphf-server import` (offline) restores them: {}",
            response.offline.join(", ")
        ));
    }
    Ok(lines.join("\n"))
}

/// Asks the core at `address` to reload its settings, as the observer when
/// `token` is set.
pub(crate) async fn reload(address: String, token: Option<String>) -> Result<String> {
    let mut client = connect(&address).await?;
    let request = request(ReloadRequest { actor: None }, token.as_deref())?;
    let response = client
        .reload(request)
        .await
        .map_err(|status| anyhow::anyhow!("Reload failed: {}", status.message()))?
        .into_inner();
    Ok(format!(
        "reloaded: {} overrides, {} ignored",
        response.overrides, response.ignored
    ))
}

/// `wuphf settings`, from the command line.
pub(crate) async fn run(config: &Path, command: SettingsSubcommand) -> Result<()> {
    match command {
        SettingsSubcommand::Local(action) => {
            let (_storage, store) = open(config, WuphfSchema::new()).await?;
            let outcome = run_local(&store, action).await?;
            println!("{}", outcome.output);
            if outcome.changed {
                eprintln!("{RELOAD_NOTE}");
            }
        }
        SettingsSubcommand::Reload { address } => {
            // The token comes from the environment, never a flag, so it stays
            // out of `ps`.
            let token = std::env::var("WUPHF_OBSERVER_TOKEN").ok();
            println!("{}", reload(address, token).await?);
        }
        SettingsSubcommand::Export { address, output } => {
            let token = std::env::var("WUPHF_OBSERVER_TOKEN").ok();
            let backup = export_online(&address, token.as_deref()).await?;
            crate::backup::write_backup(output.as_ref(), &backup)?;
            if let Some(output) = &output {
                eprintln!("wrote {}", output.display());
            }
        }
        SettingsSubcommand::Import {
            file,
            address,
            dry_run,
        } => {
            let token = std::env::var("WUPHF_OBSERVER_TOKEN").ok();
            let backup = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            println!(
                "{}",
                import_online(&address, token.as_deref(), backup, dry_run).await?
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
