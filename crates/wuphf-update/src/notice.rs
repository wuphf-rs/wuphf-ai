//! The owner's update notice: what the scheduled check says when a source has
//! a release newer than what is installed, once per release.

use std::collections::{BTreeMap, HashMap};

use semver::Version;

use crate::plan::{Plan, SourcePlan};
use crate::version::Installed;

/// Remembers which releases the owner was told about. In memory, like the
/// connectors' health notices: a restart tells again, once.
#[derive(Debug)]
pub struct Announcer {
    /// The command the notice says to run.
    command: String,
    told: HashMap<String, Version>,
}

impl Announcer {
    /// An announcer whose notices say to run `command` (the update with this
    /// core's config, so its own sources are kept).
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            told: HashMap::new(),
        }
    }

    /// The notices `plan` holds that were not sent yet, each remembered as
    /// sent.
    pub fn news(&mut self, plan: &Plan) -> Vec<String> {
        plan.sources
            .iter()
            .filter_map(|source| {
                let (version, text) = announcement(source, &self.command)?;
                (self.told.get(&source.source.name) != Some(&version)).then(|| {
                    self.told.insert(source.source.name.clone(), version);
                    text
                })
            })
            .collect()
    }
}

/// The notice for one source: its release, when that is newer than an
/// installed binary's known version. A binary whose version is unknown (a dev
/// build) is no reason to tell the owner anything.
pub fn announcement(plan: &SourcePlan, command: &str) -> Option<(Version, String)> {
    let release = plan.release()?;
    // The outdated binaries, by the version they are at.
    let mut outdated: BTreeMap<&Version, Vec<&str>> = BTreeMap::new();
    for part in &plan.parts {
        if let Installed::Version(version) = &part.installed
            && version < &release.version
        {
            outdated.entry(version).or_default().push(&part.binary);
        }
    }
    if outdated.is_empty() {
        return None;
    }
    let known = plan
        .parts
        .iter()
        .filter(|part| part.installed.version().is_some())
        .count();
    // Every known binary at one version: just the version.
    let installed = if let [(version, binaries)] = outdated.iter().collect::<Vec<_>>()[..]
        && binaries.len() == known
    {
        version.to_string()
    } else {
        outdated
            .iter()
            .map(|(version, binaries)| format!("{version} ({})", binaries.join(", ")))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = format!(
        "⬆️ {} {} is out (installed: {installed}). Update with: {command}",
        plan.source.name, release.version
    );
    if let Some(page) = &release.page {
        text.push('\n');
        text.push_str(page);
    }
    Some((release.version.clone(), text))
}

#[cfg(test)]
mod tests;
