//! `wuphf settings`: view and change the runtime settings from a terminal.
//!
//! The local actions open the same settings store the core uses, over the
//! core's database, so every change is validated exactly as at runtime and
//! the command works while the core is down. `reload` asks a running core to
//! re-read the stored settings through its settings service.

use anyhow::{Context, Result};
use botconf::cli::SettingsAction;
use botconf_grpc::proto::{ReloadRequest, settings_client::SettingsClient};
use clap::Subcommand;
use std::path::Path;
use std::sync::Arc;
use wuphf_runtime::connector_settings::ConnectorSettings;
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
    /// Ask a running core to re-read its stored settings. Edits to the config
    /// file still need a restart. Reads the token from WUPHF_OBSERVER_TOKEN.
    Reload {
        #[arg(long, default_value = "http://127.0.0.1:50051")]
        address: String,
    },
}

/// The core's database and the settings store over it, built from the config
/// file and the environment exactly as `serve` builds them, without starting
/// any server. Connectors' stored settings declarations are restored, so their
/// sections can be edited while they are down.
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
    let settings = wuphf_settings::load(schema, base, SettingsStorage::new(Arc::clone(&storage)))
        .await
        .context("invalid configuration")?;
    let settings = Arc::new(settings);
    ConnectorSettings::new(Arc::clone(&settings), storage.clone())
        .restore()
        .await
        .context("failed to restore connector settings declarations")?;
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

/// Asks the core at `address` to reload its settings, as the observer when
/// `token` is set.
pub(crate) async fn reload(address: String, token: Option<String>) -> Result<String> {
    let mut client = SettingsClient::connect(address.clone())
        .await
        .with_context(|| format!("failed to connect to {address}"))?;
    let mut request = tonic::Request::new(ReloadRequest { actor: None });
    if let Some(token) = token {
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .context("WUPHF_OBSERVER_TOKEN is not a valid header value")?,
        );
    }
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
    }
    Ok(())
}

#[cfg(test)]
mod tests;
