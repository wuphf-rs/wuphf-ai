//! Versions: this build's, a release tag's, and what an installed binary says
//! it is (`<binary> --version`).

use std::path::Path;
use std::time::Duration;

use semver::Version;

/// How long a binary may take to print its version.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// This build's version: the workspace's, every wuphf binary's.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the workspace version is semver")
}

/// A tag's version: `v1.2.3`, or `1.2.3`.
pub fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// The version in a binary's `--version` output: its first word that is one
/// (`wuphf-telegram 1.2.3`, `my-misc v1.2.3 (abc123)`).
pub fn reported(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(parse_tag)
}

/// What is installed under a binary's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Nothing.
    Missing,
    /// A binary that did not say a version (`dev` builds, or one without
    /// `--version`); what it said, or why it said nothing.
    Unknown(String),
    Version(Version),
}

impl Installed {
    pub fn version(&self) -> Option<&Version> {
        match self {
            Installed::Version(version) => Some(version),
            _ => None,
        }
    }
}

impl std::fmt::Display for Installed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Installed::Missing => f.write_str("not installed"),
            Installed::Unknown(_) => f.write_str("unknown"),
            Installed::Version(version) => write!(f, "{version}"),
        }
    }
}

/// Ask the binary at `path` for its version.
pub async fn probe(path: &Path) -> Installed {
    if !path.is_file() {
        return Installed::Missing;
    }
    let run = tokio::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Err(_) => Installed::Unknown("--version did not finish".into()),
        Ok(Err(error)) => Installed::Unknown(format!("could not run it: {error}")),
        Ok(Ok(output)) => {
            let said = String::from_utf8_lossy(&output.stdout);
            match reported(&said) {
                Some(version) => Installed::Version(version),
                None => Installed::Unknown(said.trim().to_owned()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_read_from_what_a_binary_prints() {
        assert_eq!(
            reported("wuphf-telegram 1.2.3\n"),
            Some(Version::new(1, 2, 3))
        );
        assert_eq!(
            reported("my-misc v0.4.0 (abc123)"),
            Some(Version::new(0, 4, 0))
        );
        assert_eq!(
            reported("wuphf-server 1.0.0-rc.1"),
            Some(Version::parse("1.0.0-rc.1").unwrap())
        );
        assert_eq!(reported("wuphf-whatsapp dev"), None);
        assert_eq!(reported(""), None);
    }

    #[test]
    fn a_tag_is_a_version_with_or_without_its_v() {
        assert_eq!(parse_tag("v0.2.0"), Some(Version::new(0, 2, 0)));
        assert_eq!(parse_tag("0.2.0"), Some(Version::new(0, 2, 0)));
        assert_eq!(parse_tag("nightly"), None);
    }

    #[tokio::test]
    async fn an_installed_binary_is_asked_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            crate::install::make_executable(&path).unwrap();
            path
        };
        assert_eq!(
            probe(&script("versioned", "echo \"versioned 2.0.1\"")).await,
            Installed::Version(Version::new(2, 0, 1))
        );
        assert_eq!(
            probe(&script("dev", "echo \"dev dev\"")).await,
            Installed::Unknown("dev dev".into())
        );
        assert_eq!(probe(&dir.path().join("absent")).await, Installed::Missing);
    }
}
