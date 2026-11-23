//! Versions and updates for every wuphf binary.
//!
//! An [`Updater`] knows the release sources (wuphf's own and the configured
//! ones, see [`config::UpdatesConfig`]), where the binaries are installed, and
//! how to tell what is installed. [`Updater::plan`] asks each source for its
//! newest release; [`Updater::apply`] downloads (or builds) and checks every
//! binary an update replaces before installing any, installs them, and
//! restarts the units that were running. `wuphf-server update` drives it, and
//! `serve`'s scheduled check turns a plan into the owner's notice
//! ([`notice::Announcer`]).

pub mod archive;
pub mod config;
pub mod forge;
pub mod from_source;
pub mod http;
pub mod install;
pub mod notice;
pub mod plan;
pub mod services;
pub mod source;
pub mod verify;
pub mod version;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

/// Versions, as releases and binaries say them.
pub use semver::Version;

use crate::config::{Origin, UpdatesConfig};
use crate::forge::Release;
use crate::http::Fetch;
use crate::install::{Installer, Lock};
use crate::plan::{Action, Part, Plan, SourcePlan, decide};
use crate::services::Services;
use crate::source::{Source, Verification, WUPHF};
use crate::version::Installed;

/// Reads an environment variable (the token variables a source names).
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A binary larger than this is not one.
const BINARY_LIMIT: u64 = 512 * 1024 * 1024;
/// Nor is a checksums file or a signature larger than this.
const CHECKSUMS_LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Http(String),
    #[error("{0}")]
    Forge(String),
    #[error("{0}")]
    Verify(String),
    #[error("{0}")]
    Install(String),
    #[error("{0}")]
    Source(String),
    #[error("{0}")]
    Restart(String),
}

/// What [`Updater::plan`] looks at.
#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Build every source from its source, whatever it is configured with.
    pub from_source: bool,
    /// Only these binaries (none: all). A binary named here is installed even
    /// when it is not yet.
    pub only: Vec<String>,
}

/// What [`Updater::apply`] or [`Updater::rollback`] did.
#[derive(Debug, Clone, Default)]
pub struct Applied {
    /// The binaries installed: name, from, to.
    pub installed: Vec<(String, Installed, String)>,
    /// The units restarted.
    pub restarted: Vec<String>,
    /// The units that failed to restart, and why.
    pub restart_failures: Vec<(String, String)>,
}

/// A binary fetched and checked, waiting to be installed.
struct Staged {
    binary: String,
    contents: Vec<u8>,
    from: Installed,
    to: String,
    unit: Option<String>,
}

pub struct Updater {
    sources: Vec<Source>,
    installer: Installer,
    prerelease: bool,
    http: Arc<dyn Fetch>,
}

impl Updater {
    /// An updater for `config`, reading token variables through `env`. The
    /// install directory defaults to this executable's.
    pub fn new(config: &UpdatesConfig, env: Lookup, http: Arc<dyn Fetch>) -> Result<Self, Error> {
        let install_dir = match &config.install_dir {
            Some(dir) => dir.clone(),
            None => running_dir()?,
        };
        Ok(Self {
            sources: source::resolve(config, env)?,
            installer: Installer::new(install_dir, config.state_dir.clone()),
            prerelease: config.prerelease,
            http,
        })
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    pub fn installer(&self) -> &Installer {
        &self.installer
    }

    /// Each source's newest release and what it means for each installed
    /// binary. A source that cannot be asked says why in its plan; the others
    /// still plan.
    pub async fn plan(&self, options: &PlanOptions) -> Result<Plan, Error> {
        let known: HashSet<&str> = self
            .sources
            .iter()
            .flat_map(|source| source.binaries.iter().map(String::as_str))
            .collect();
        if let Some(unknown) = options
            .only
            .iter()
            .find(|binary| !known.contains(binary.as_str()))
        {
            return Err(Error::Config(format!(
                "no update source provides {unknown}"
            )));
        }
        let mut sources = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            let binaries: Vec<&String> = source
                .binaries
                .iter()
                .filter(|binary| options.only.is_empty() || options.only.contains(binary))
                .collect();
            if binaries.is_empty() {
                continue;
            }
            let origin = if options.from_source {
                Origin::Source
            } else {
                source.origin
            };
            let latest = self
                .latest(source, origin)
                .await
                .map_err(|error| error.to_string());
            let latest_version = latest
                .as_ref()
                .ok()
                .and_then(|release| release.as_ref().map(|release| &release.version));
            let mut parts = Vec::with_capacity(binaries.len());
            for binary in binaries {
                let installed = version::probe(&self.installer.path(binary)).await;
                let action = match decide(&installed, latest_version) {
                    // Named, it is installed: how a new host gets its parts.
                    Action::NotInstalled if options.only.contains(binary) => Action::Update,
                    action => action,
                };
                parts.push(Part {
                    action,
                    binary: binary.clone(),
                    installed,
                });
            }
            sources.push(SourcePlan {
                source: source.clone(),
                origin,
                latest,
                parts,
            });
        }
        Ok(Plan { sources })
    }

    async fn latest(&self, source: &Source, origin: Origin) -> Result<Option<Release>, Error> {
        let releases = match origin {
            Origin::Release => {
                forge::releases(
                    self.http.as_ref(),
                    &source.repository,
                    source.token.as_deref(),
                )
                .await?
            }
            Origin::Source => forge::tags(&source.repository).await?,
        };
        Ok(forge::latest(releases, self.prerelease))
    }

    /// Install what `plan` updates: every binary fetched (or built) and
    /// checked first, then all installed (a failure puts back the ones already
    /// replaced), then the units that were running restarted, unless
    /// `services` is none. `progress` hears each step.
    pub async fn apply(
        &self,
        plan: &Plan,
        services: Option<&dyn Services>,
        progress: &(dyn Fn(&str) + Sync),
    ) -> Result<Applied, Error> {
        let lock = self.installer.lock()?;
        let mut staged = Vec::new();
        for source in &plan.sources {
            let binaries: Vec<String> = source.updates().map(|part| part.binary.clone()).collect();
            let Some(release) = source.release().filter(|_| !binaries.is_empty()) else {
                continue;
            };
            let fetched = match source.origin {
                Origin::Release => {
                    self.download(&source.source, release, &binaries, progress)
                        .await?
                }
                Origin::Source => {
                    progress(&format!(
                        "building {} {} from source",
                        source.source.name, release.tag
                    ));
                    from_source::build(
                        &source.source,
                        release,
                        &binaries,
                        self.installer.state_dir(),
                    )
                    .await?
                }
            };
            for (binary, contents) in fetched {
                if source.source.name == WUPHF {
                    self.check_version(&binary, &contents, release).await?;
                }
                let from = source
                    .parts
                    .iter()
                    .find(|part| part.binary == binary)
                    .map(|part| part.installed.clone())
                    .unwrap_or(Installed::Missing);
                let unit = source.source.service(&binary).map(str::to_owned);
                staged.push(Staged {
                    binary,
                    contents,
                    from,
                    to: release.version.to_string(),
                    unit,
                });
            }
        }
        if staged.is_empty() {
            return Ok(Applied::default());
        }

        // Only what runs now is restarted: an update never starts a unit.
        let mut running = Vec::new();
        if let Some(services) = services {
            for unit in staged.iter().filter_map(|staged| staged.unit.as_deref()) {
                if !running.iter().any(|known: &String| known == unit)
                    && services.is_active(unit).await
                {
                    running.push(unit.to_owned());
                }
            }
        }

        self.installer.begin(&lock)?;
        let mut applied = Applied::default();
        for staged in staged {
            progress(&format!("installing {} {}", staged.binary, staged.to));
            if let Err(error) = self
                .installer
                .replace(&lock, &staged.binary, &staged.contents)
            {
                self.undo(&lock, &applied, progress);
                return Err(error);
            }
            applied
                .installed
                .push((staged.binary, staged.from, staged.to));
        }

        if let Some(services) = services {
            self.restart(services, &running, &mut applied, progress)
                .await;
        }
        Ok(applied)
    }

    /// Put back what the last update replaced, then restart the units that
    /// were running, unless `services` is none.
    pub async fn rollback(
        &self,
        services: Option<&dyn Services>,
        progress: &(dyn Fn(&str) + Sync),
    ) -> Result<Applied, Error> {
        let lock = self.installer.lock()?;
        let binaries = self.installer.replaced()?;
        let units: Vec<String> = self
            .sources
            .iter()
            .flat_map(|source| {
                source
                    .binaries
                    .iter()
                    .filter(|binary| binaries.contains(binary))
                    .filter_map(|binary| source.service(binary))
            })
            .map(str::to_owned)
            .collect();
        let mut running = Vec::new();
        if let Some(services) = services {
            for unit in units {
                if !running.contains(&unit) && services.is_active(&unit).await {
                    running.push(unit);
                }
            }
        }
        let mut applied = Applied::default();
        for binary in &binaries {
            progress(&format!("putting back the previous {binary}"));
            let from = version::probe(&self.installer.path(binary)).await;
            self.installer.rollback(&lock, binary)?;
            let to = version::probe(&self.installer.path(binary)).await;
            applied
                .installed
                .push((binary.clone(), from, to.to_string()));
        }
        if let Some(services) = services {
            self.restart(services, &running, &mut applied, progress)
                .await;
        }
        Ok(applied)
    }

    /// Download and check `binaries` from `release`.
    async fn download(
        &self,
        source: &Source,
        release: &Release,
        binaries: &[String],
        progress: &(dyn Fn(&str) + Sync),
    ) -> Result<Vec<(String, Vec<u8>)>, Error> {
        let token = source.token.as_deref();
        let get = async |name: &str, limit: u64| -> Result<Vec<u8>, Error> {
            let asset = release.asset(name).ok_or_else(|| {
                Error::Forge(format!("{} {} has no {name}", source.name, release.tag))
            })?;
            progress(&format!("downloading {name}"));
            self.http
                .get(
                    &asset.url,
                    &source.repository.headers(&asset.url, token),
                    limit,
                )
                .await
        };
        let checksums = match &source.verification {
            Verification::None => None,
            verification => {
                let checksums = get(&source.checksums, CHECKSUMS_LIMIT).await?;
                if let Verification::Signed {
                    public_key,
                    comment,
                } = verification
                {
                    let signature =
                        get(&format!("{}.minisig", source.checksums), CHECKSUMS_LIMIT).await?;
                    let comment = comment
                        .as_ref()
                        .map(|comment| comment.replace("{tag}", &release.tag));
                    verify::check_signature(
                        &checksums,
                        &String::from_utf8_lossy(&signature),
                        public_key,
                        comment.as_deref(),
                    )?;
                }
                Some(verify::parse_checksums(&String::from_utf8_lossy(
                    &checksums,
                )))
            }
        };
        let mut fetched = Vec::with_capacity(binaries.len());
        for binary in binaries {
            let name = source.asset_name(binary, &release.tag);
            let download = get(&name, BINARY_LIMIT).await?;
            if let Some(checksums) = &checksums {
                verify::check_sum(checksums, &name, &download)?;
            }
            fetched.push((
                binary.clone(),
                archive::binary(&name, download, binary, BINARY_LIMIT)?,
            ));
        }
        Ok(fetched)
    }

    /// A wuphf binary must say the release's version: anything else is the
    /// wrong file, or a build that would update again on every run.
    async fn check_version(
        &self,
        binary: &str,
        contents: &[u8],
        release: &Release,
    ) -> Result<(), Error> {
        let staging = self.installer.staging(binary);
        let failed =
            |error: std::io::Error| Error::Install(format!("stage {}: {error}", staging.display()));
        if let Some(dir) = staging.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        std::fs::write(&staging, contents).map_err(failed)?;
        install::make_executable(&staging).map_err(failed)?;
        let said = version::probe(&staging).await;
        let _ = std::fs::remove_file(&staging);
        match said {
            Installed::Version(version) if version == release.version => Ok(()),
            said => Err(Error::Verify(format!(
                "the new {binary} says it is {said}, not {}",
                release.version
            ))),
        }
    }

    /// Put back the binaries this update already replaced.
    fn undo(&self, lock: &Lock, applied: &Applied, progress: &(dyn Fn(&str) + Sync)) {
        for (binary, ..) in &applied.installed {
            progress(&format!("putting back the previous {binary}"));
            if let Err(error) = self.installer.rollback(lock, binary) {
                progress(&format!("could not put back {binary}: {error}"));
            }
        }
    }

    async fn restart(
        &self,
        services: &dyn Services,
        units: &[String],
        applied: &mut Applied,
        progress: &(dyn Fn(&str) + Sync),
    ) {
        for unit in units {
            progress(&format!("restarting {unit}"));
            match services.restart(unit).await {
                Ok(()) => applied.restarted.push(unit.clone()),
                Err(error) => applied
                    .restart_failures
                    .push((unit.clone(), error.to_string())),
            }
        }
    }
}

/// The directory this executable is in.
fn running_dir() -> Result<PathBuf, Error> {
    let exe = std::env::current_exe()
        .map_err(|error| Error::Config(format!("find this executable: {error}")))?;
    exe.parent()
        .map(PathBuf::from)
        .ok_or_else(|| Error::Config(format!("{} has no directory", exe.display())))
}

#[cfg(test)]
mod tests;
