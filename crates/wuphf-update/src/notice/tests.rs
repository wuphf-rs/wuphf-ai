use semver::Version;

use super::*;
use crate::config::{Origin, UpdatesConfig};
use crate::forge::Release;
use crate::plan::{Action, Part, Plan, SourcePlan};
use crate::source::resolve;

fn plan(latest: &str, installed: &[(&str, Installed)]) -> Plan {
    let source = resolve(&UpdatesConfig::default(), &|_| None)
        .unwrap()
        .remove(0);
    Plan {
        sources: vec![SourcePlan {
            source,
            origin: Origin::Release,
            latest: Ok(Some(Release {
                tag: format!("v{latest}"),
                version: Version::parse(latest).unwrap(),
                page: Some(format!(
                    "https://github.com/wuphf-rs/wuphf-ai/releases/tag/v{latest}"
                )),
                assets: Vec::new(),
            })),
            parts: installed
                .iter()
                .map(|(binary, installed)| Part {
                    binary: (*binary).to_owned(),
                    installed: installed.clone(),
                    action: Action::Update,
                })
                .collect(),
        }],
    }
}

const UPDATE: &str = "sudo wuphf-server update --config /etc/wuphf/core.toml";

fn at(version: &str) -> Installed {
    Installed::Version(Version::parse(version).unwrap())
}

#[test]
fn the_owner_hears_of_a_release_once() {
    let mut announcer = Announcer::new(UPDATE);
    let outdated = plan(
        "0.2.0",
        &[
            ("wuphf-server", at("0.1.0")),
            ("wuphf-telegram", at("0.1.0")),
        ],
    );
    assert_eq!(
        announcer.news(&outdated),
        [
            "⬆️ wuphf 0.2.0 is out (installed: 0.1.0). Update with: sudo wuphf-server update --config /etc/wuphf/core.toml\n\
          https://github.com/wuphf-rs/wuphf-ai/releases/tag/v0.2.0"
        ]
    );
    assert!(announcer.news(&outdated).is_empty());
    // A newer release is news again.
    let newer = plan("0.3.0", &[("wuphf-server", at("0.1.0"))]);
    assert_eq!(announcer.news(&newer).len(), 1);
}

#[test]
fn binaries_at_different_versions_are_listed_by_version() {
    let (_, text) = announcement(
        &plan(
            "0.3.0",
            &[
                ("wuphf-server", at("0.2.0")),
                ("wuphf-telegram", at("0.1.0")),
                ("wuphf-misc", at("0.3.0")),
            ],
        )
        .sources[0],
        UPDATE,
    )
    .unwrap();
    assert!(
        text.contains("(installed: 0.1.0 (wuphf-telegram), 0.2.0 (wuphf-server))"),
        "{text}"
    );
}

#[test]
fn nothing_is_said_when_everything_is_current_or_unknown() {
    let current = plan(
        "0.2.0",
        &[
            ("wuphf-server", at("0.2.0")),
            ("wuphf-tui", Installed::Unknown("dev".into())),
        ],
    );
    assert_eq!(announcement(&current.sources[0], UPDATE), None);
    assert!(Announcer::new(UPDATE).news(&current).is_empty());
}
