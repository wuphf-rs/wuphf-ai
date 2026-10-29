//! Loading: the TOML file, overridden by `WUPHF_CORE_*` environment
//! variables, then the settings store on top.

use crate::schema::{SettingsStore, WuphfSchema};
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Prefix of environment variables overriding the file, e.g.
/// `WUPHF_CORE_LISTEN`; `__` separates levels
/// (`WUPHF_CORE_FUNCTIONS__MAX_DEADLINE_MS`). It is not the bare `WUPHF_`
/// because the core's environment also holds tokens and connector variables
/// (`WUPHF_INVOKE_TOKEN`, `WUPHF_TELEGRAM_APP_ID`, …) that are not config keys.
pub const ENV_PREFIX: &str = "WUPHF_CORE_";

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("configuration file {} does not exist", .0.display())]
    NotFound(PathBuf),
}

/// The file, then the environment. Fails if the file is missing.
pub fn base(path: &Path) -> Result<Figment, LoadError> {
    if !path.is_file() {
        return Err(LoadError::NotFound(path.to_path_buf()));
    }
    Ok(Figment::new()
        .merge(Toml::file_exact(path))
        .merge(Env::prefixed(ENV_PREFIX).split("__")))
}

/// The file and the environment, read again on every reload, so an edited
/// config file applies on `reload()`. Startup checks that need the values
/// once (the database URL) use [`base`].
pub fn source(path: PathBuf) -> impl botconf::BaseSource + Clone {
    move || base(&path)
}

/// The settings store over `base`, with `storage` holding the overrides. A
/// fixed [`Figment`] base is read once; a [`source`] is read again on every
/// reload.
pub async fn load(
    schema: WuphfSchema,
    base: impl botconf::BaseSource,
    storage: impl botconf::Storage,
) -> Result<SettingsStore, botconf::SettingsError> {
    SettingsStore::load(schema, base, storage).await
}
