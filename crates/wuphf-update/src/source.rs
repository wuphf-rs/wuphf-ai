//! Release sources, resolved from the configuration: wuphf's own (its binaries
//! built in) and the configured ones, each binary belonging to exactly one.

use std::collections::{BTreeMap, HashSet};

use crate::config::{Forge, Origin, SourceConfig, UpdatesConfig, Verify};
use crate::forge::Repository;
use crate::{Error, Lookup};

/// wuphf's own release key (`release/minisign.pub`). Empty until the first
/// signed release: a signature then cannot be checked, so nothing installs.
const WUPHF_PUBLIC_KEY: &str = include_str!("../../../release/minisign.pub");

/// wuphf's binaries, in restart order (the core first, so the parts reconnect
/// to the new one), each with the systemd unit it runs as.
pub const WUPHF_BINARIES: &[(&str, Option<&str>)] = &[
    ("wuphf-server", Some("wuphf-core.service")),
    ("wuphf-tui", None),
    ("wuphf-whatsapp-rust", Some("wuphf-whatsapp-rust.service")),
    ("wuphf-telegram", Some("wuphf-telegram.service")),
    ("wuphf-whatsapp", Some("wuphf-whatsapp.service")),
    ("wuphf-instagram", Some("wuphf-instagram.service")),
    ("wuphf-mediaconv", Some("wuphf-mediaconv.service")),
    ("wuphf-misc", Some("wuphf-misc.service")),
];

/// wuphf's own build: every binary, named as installed, into `$OUT`.
const WUPHF_BUILD: &str = r#"scripts/build-release.sh "$OUT""#;

/// The name wuphf's own source goes by.
pub const WUPHF: &str = "wuphf";

/// A source of releases for some binaries.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub repository: Repository,
    /// The binaries it provides, in restart order.
    pub binaries: Vec<String>,
    /// The asset pattern (see [`SourceConfig::asset`]).
    pub asset: String,
    /// The checksums asset.
    pub checksums: String,
    pub verification: Verification,
    pub origin: Origin,
    /// The build command for [`Origin::Source`].
    pub build: Option<String>,
    /// Each binary's systemd unit; a binary without one is not restarted.
    pub services: BTreeMap<String, String>,
    /// The forge API token.
    pub token: Option<String>,
}

/// What a source's downloads must pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// The checksums signed by `public_key` (a `.pub` file or its key line),
    /// their trusted comment equal to `comment` (with `{tag}` filled in) when
    /// one is set; then the download's sum.
    Signed {
        public_key: String,
        comment: Option<String>,
    },
    /// The download's sum in the checksums.
    Checksums,
    /// Nothing.
    None,
}

impl Source {
    /// The asset name holding `binary` in the release `tag`, for this host.
    pub fn asset_name(&self, binary: &str, tag: &str) -> String {
        let platform = Platform::current();
        self.asset
            .replace("{binary}", binary)
            .replace("{tag}", tag)
            .replace("{version}", tag.strip_prefix('v').unwrap_or(tag))
            .replace("{os}", platform.os)
            .replace("{arch}", platform.arch)
            .replace("{goarch}", platform.goarch)
    }

    /// The systemd unit `binary` runs as, if any.
    pub fn service(&self, binary: &str) -> Option<&str> {
        self.services.get(binary).map(String::as_str)
    }
}

/// The host's platform, as asset names spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
    pub goarch: &'static str,
}

impl Platform {
    pub fn current() -> Self {
        let arch = std::env::consts::ARCH;
        Self {
            os: std::env::consts::OS,
            arch,
            goarch: match arch {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                "x86" => "386",
                other => other,
            },
        }
    }
}

/// Every source the configuration names, wuphf's own first (when enabled),
/// each binary in exactly one: a configured source's binaries are left out of
/// wuphf's own.
pub fn resolve(config: &UpdatesConfig, env: Lookup) -> Result<Vec<Source>, Error> {
    let mut claimed = HashSet::new();
    let mut names = HashSet::from([WUPHF.to_owned()]);
    let mut configured = Vec::with_capacity(config.sources.len());
    for source in &config.sources {
        if !names.insert(source.name.clone()) {
            return Err(Error::Config(format!(
                "two update sources are named {:?}",
                source.name
            )));
        }
        for binary in &source.binaries {
            if !claimed.insert(binary.clone()) {
                return Err(Error::Config(format!("{binary} is in two update sources")));
            }
        }
        configured.push(configured_source(source, env)?);
    }

    let mut sources = Vec::with_capacity(configured.len() + 1);
    if config.wuphf.enabled {
        let official = &config.wuphf;
        let (binaries, services) = WUPHF_BINARIES
            .iter()
            .filter(|(binary, _)| !claimed.contains(*binary))
            .fold(
                (Vec::new(), BTreeMap::new()),
                |(mut binaries, mut services), (binary, service)| {
                    binaries.push((*binary).to_owned());
                    if let Some(service) = service {
                        services.insert((*binary).to_owned(), (*service).to_owned());
                    }
                    (binaries, services)
                },
            );
        sources.push(Source {
            name: WUPHF.to_owned(),
            repository: Repository::parse(&official.repository, Some(Forge::Github))?,
            binaries,
            asset: "{binary}-{os}-{arch}".to_owned(),
            checksums: "SHA256SUMS".to_owned(),
            verification: Verification::Signed {
                public_key: official
                    .public_key
                    .clone()
                    .unwrap_or_else(|| WUPHF_PUBLIC_KEY.to_owned()),
                // The release workflow signs with "wuphf <tag>".
                comment: Some("wuphf {tag}".to_owned()),
            },
            origin: official.from,
            build: Some(WUPHF_BUILD.to_owned()),
            services,
            token: token(official.token_env.as_deref(), env)?,
        });
    }
    sources.extend(configured);
    Ok(sources)
}

fn configured_source(source: &SourceConfig, env: Lookup) -> Result<Source, Error> {
    let context =
        |message: String| Error::Config(format!("update source {}: {message}", source.name));
    if source.binaries.is_empty() {
        return Err(context("lists no binaries".into()));
    }
    if let Some(binary) = source.binaries.iter().find(|binary| !is_file_name(binary)) {
        return Err(context(format!("{binary:?} is not a file name")));
    }
    if source.from == Origin::Source && source.build.is_none() {
        return Err(context("from = \"source\" needs a build command".into()));
    }
    let verification = match (source.verify, &source.public_key) {
        (Some(Verify::Signed) | None, Some(public_key)) => Verification::Signed {
            public_key: public_key.clone(),
            comment: None,
        },
        (Some(Verify::Signed), None) => {
            return Err(context("verify = \"signed\" needs a public_key".into()));
        }
        (Some(Verify::Checksums) | None, _) => Verification::Checksums,
        (Some(Verify::None), _) => Verification::None,
    };
    let services = source
        .binaries
        .iter()
        .map(|binary| {
            let unit = source
                .services
                .get(binary)
                .cloned()
                .unwrap_or_else(|| format!("{binary}.service"));
            (binary.clone(), unit)
        })
        .collect();
    Ok(Source {
        name: source.name.clone(),
        repository: Repository::parse(&source.repository, source.forge)
            .map_err(|error| context(error.to_string()))?,
        binaries: source.binaries.clone(),
        asset: source.asset.clone(),
        checksums: source.checksums.clone(),
        verification,
        origin: source.from,
        build: source.build.clone(),
        services,
        token: token(source.token_env.as_deref(), env)?,
    })
}

fn token(name: Option<&str>, env: Lookup) -> Result<Option<String>, Error> {
    let Some(name) = name else {
        return Ok(None);
    };
    match env(name) {
        Some(value) if !value.is_empty() => Ok(Some(value)),
        _ => Err(Error::Config(format!(
            "environment variable {name} (token_env) is unset or empty"
        ))),
    }
}

/// A plain file name: no directories, nothing hidden.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\0')
}

#[cfg(test)]
mod tests;
