//! What an update would do: per source, its newest release and, per binary,
//! whether that is an update.

use semver::Version;

use crate::config::Origin;
use crate::forge::Release;
use crate::source::Source;
use crate::version::Installed;

/// Every source's newest release and what it means for each binary.
#[derive(Debug, Clone)]
pub struct Plan {
    pub sources: Vec<SourcePlan>,
}

#[derive(Debug, Clone)]
pub struct SourcePlan {
    pub source: Source,
    /// Where its binaries come from this time (`--from-source` overrides the
    /// source's own).
    pub origin: Origin,
    /// Its newest release, none when it has none, or why it could not be
    /// asked.
    pub latest: Result<Option<Release>, String>,
    /// Its binaries, in restart order.
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub binary: String,
    pub installed: Installed,
    pub action: Action,
}

/// What an update does to one binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Installs the newest release: it is newer, or the installed version is
    /// unknown.
    Update,
    /// Nothing: the newest release is installed.
    Current,
    /// Nothing: what is installed is newer than any release.
    Ahead,
    /// Nothing: the binary is not installed (an update only replaces).
    NotInstalled,
    /// Nothing: the source has no release, or could not be asked.
    NoRelease,
}

/// What an update does to a binary given what is installed and the newest
/// release.
pub fn decide(installed: &Installed, latest: Option<&Version>) -> Action {
    let Some(latest) = latest else {
        return Action::NoRelease;
    };
    match installed {
        Installed::Missing => Action::NotInstalled,
        Installed::Unknown(_) => Action::Update,
        Installed::Version(version) if version < latest => Action::Update,
        Installed::Version(version) if version == latest => Action::Current,
        Installed::Version(_) => Action::Ahead,
    }
}

impl Plan {
    /// Whether anything would be installed.
    pub fn has_updates(&self) -> bool {
        self.sources
            .iter()
            .any(|source| source.updates().next().is_some())
    }
}

impl SourcePlan {
    /// The release an update installs, when there is one.
    pub fn release(&self) -> Option<&Release> {
        self.latest.as_ref().ok()?.as_ref()
    }

    /// The binaries an update replaces.
    pub fn updates(&self) -> impl Iterator<Item = &Part> {
        self.parts
            .iter()
            .filter(|part| part.action == Action::Update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_is_updated_when_the_release_is_newer_or_its_version_unknown() {
        let latest = Version::new(1, 2, 0);
        let installed = |major, minor| Installed::Version(Version::new(major, minor, 0));
        assert_eq!(decide(&installed(1, 1), Some(&latest)), Action::Update);
        assert_eq!(decide(&installed(1, 2), Some(&latest)), Action::Current);
        assert_eq!(decide(&installed(1, 3), Some(&latest)), Action::Ahead);
        assert_eq!(
            decide(&Installed::Unknown("dev".into()), Some(&latest)),
            Action::Update
        );
        assert_eq!(
            decide(&Installed::Missing, Some(&latest)),
            Action::NotInstalled
        );
        assert_eq!(decide(&installed(1, 1), None), Action::NoRelease);
    }

    #[test]
    fn a_release_candidate_is_older_than_its_release() {
        let candidate = Installed::Version(Version::parse("1.2.0-rc.1").unwrap());
        assert_eq!(
            decide(&candidate, Some(&Version::new(1, 2, 0))),
            Action::Update
        );
    }
}
