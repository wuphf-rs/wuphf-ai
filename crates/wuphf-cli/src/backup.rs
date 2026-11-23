//! `wuphf-server export` / `import`: one backup of the core's settings, its
//! config file and every stored override (its own runtime keys and every
//! connector's, `connector_settings.<id>`), restored offline (see
//! `botconf::export`). `wuphf-server settings export` / `import` do the same
//! through a running core, without secrets.
//!
//! A running core holds its database's lock, and an import refuses while it
//! does: the core would keep its own copy of the settings and write over the
//! restored ones.

use anyhow::{Context, Result, anyhow};
use botconf::Schema as _;
use botconf::env_file::EnvFile;
use botconf::export::{self, Backup, ImportTarget, Imported};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wuphf_connector::session_lock::{SessionLock, SessionLockError};
use wuphf_settings::{Configuration, SettingsStorage, WuphfSchema};
use wuphf_storage::SqliteStorage;

/// Takes the lock of the database `database_url` names, as a running core
/// holds it; `None` for an in-memory database.
pub(crate) fn lock_database(database_url: &str) -> Result<Option<SessionLock>> {
    let Some(file) = SqliteStorage::database_file(database_url) else {
        return Ok(None);
    };
    match SessionLock::acquire(&file) {
        Ok(lock) => Ok(Some(lock)),
        Err(SessionLockError::Held(_)) => Err(anyhow!(
            "a core is running on {}: stop it first",
            file.display()
        )),
        Err(error) => Err(error.into()),
    }
}

/// The env file's variables, then the process environment's.
fn env_lookup(env_file: &Path) -> Result<impl Fn(&str) -> Option<String> + Send + Sync + 'static> {
    let vars = EnvFile::parse(&read_optional(env_file)?).vars();
    Ok(move |name: &str| vars.get(name).cloned().or_else(|| std::env::var(name).ok()))
}

fn read_optional(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// The backup of the settings the config file at `config` and its database
/// hold; with `secrets`, the variables its options name are read from
/// `env_file`, then the environment.
pub(crate) async fn export_offline(
    config: &Path,
    env_file: &Path,
    secrets: bool,
) -> Result<String> {
    // The tokens as the core reads them, the env file first: `.env.local`
    // need not be exported to the shell.
    let schema = WuphfSchema::with_env(env_lookup(env_file)?);
    let (_storage, store) = crate::settings::open(config, schema).await?;
    let lookup = env_lookup(env_file)?;
    let lookup: &dyn Fn(&str) -> Option<String> = &lookup;
    Ok(export::export(
        &wuphf_settings::description(),
        &store.current(),
        secrets.then_some(lookup),
    )?)
}

/// Restores the backup `text` offline: the config file at `config` (and the
/// env file, when the backup has secrets), and the overrides in the database
/// the restored file names. Refused while a core runs on that database.
pub(crate) async fn import_offline(config: &Path, env_file: &Path, text: &str) -> Result<Imported> {
    let backup = Backup::parse(text)?;
    let database_url = backup
        .config
        .get("database_url")
        .and_then(|url| url.as_str())
        .map(str::to_owned)
        .context("the backup names no database_url")?;
    let _lock = lock_database(&database_url)?;
    let storage = Arc::new(
        SqliteStorage::connect(&database_url)
            .await
            .with_context(|| format!("failed to open {database_url}"))?,
    );
    let namespaces = WuphfSchema::new().dynamic_namespaces();
    let target = ImportTarget {
        config,
        env_file,
        storage: &SettingsStorage::new(storage),
        namespaces: &namespaces,
    };
    // The tokens are checked as the core will read them: the env file being
    // written, then the environment.
    let imported = export::import(
        &wuphf_settings::description(),
        &backup,
        &target,
        |file, env| {
            let env: BTreeMap<String, String> = env.clone();
            wuphf_settings::check(file, move |name| {
                env.get(name).cloned().or_else(|| std::env::var(name).ok())
            })
        },
    )
    .await?;
    Ok(imported)
}

/// What an offline import did, for the terminal.
pub(crate) fn imported_summary(config: &Path, imported: &Imported) -> String {
    let mut lines = vec![format!(
        "restored {}: {} stored overrides removed, {} restored{}",
        config.display(),
        imported.removed,
        imported.stored,
        if imported.env_vars > 0 {
            format!(", {} env file variables set", imported.env_vars)
        } else {
            String::new()
        }
    )];
    for kept in &imported.kept {
        lines.push(format!("kept the previous file as {}", kept.display()));
    }
    lines.join("\n")
}

/// Writes a backup to `output` (a new file is readable by its owner only: it
/// may hold secrets), or prints it.
pub(crate) fn write_backup(output: Option<&PathBuf>, text: &str) -> Result<()> {
    let Some(output) = output else {
        print!("{text}");
        return Ok(());
    };
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(output)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .with_context(|| format!("failed to write {}", output.display()))
}

/// The database URL of the config file at `config`, for `serve`'s lock.
pub(crate) fn database_url(config: &Path) -> Result<String> {
    let file: Configuration = wuphf_settings::base(config)?
        .extract()
        .context("invalid configuration")?;
    Ok(file.database_url)
}

#[cfg(test)]
mod tests;
