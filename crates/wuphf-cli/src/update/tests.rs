use wuphf_update::Version;
use wuphf_update::config::Origin;
use wuphf_update::forge::Release;
use wuphf_update::plan::{Part, SourcePlan};
use wuphf_update::source::resolve;
use wuphf_update::version::Installed;

use super::*;

fn config_file(toml: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wuphf-update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("core.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

#[test]
fn only_the_updates_section_is_read() {
    // The rest of the file is not checked: no connector token is needed.
    let path = config_file(
        "listen = \"nonsense\"\n\
         [updates]\n\
         check_interval_seconds = 0\n\
         [[updates.sources]]\n\
         name = \"my-misc\"\n\
         repository = \"https://codeberg.org/you/wuphf-misc\"\n\
         binaries = [\"wuphf-misc\"]\n",
    );
    let config = updates_config(&path).unwrap();
    assert_eq!(config.check_interval_seconds, 0);
    assert_eq!(config.sources[0].name, "my-misc");
    assert_eq!(config.sources[0].asset, "{binary}-{os}-{arch}");
    assert!(config.wuphf.enabled);
}

#[test]
fn a_config_without_the_section_updates_wuphfs_own_binaries() {
    let config = updates_config(&config_file("listen = \"127.0.0.1:1\"\n")).unwrap();
    assert_eq!(
        config.wuphf.repository,
        wuphf_update::config::WUPHF_REPOSITORY
    );
    assert!(config.sources.is_empty());
    // A missing file is an error, not the defaults: run without this core's
    // config, an update would not know its own sources.
    let missing = std::env::temp_dir().join("wuphf-update-no-such-file.toml");
    let error = updates_config(&missing).unwrap_err();
    assert!(error.to_string().contains("does not exist"), "{error}");
}

#[test]
fn the_plan_shows_each_installed_binary_and_what_happens_to_it() {
    let source = resolve(&UpdatesConfig::default(), &|_| None)
        .unwrap()
        .remove(0);
    let at = |version: &str| Installed::Version(Version::parse(version).unwrap());
    let part = |binary: &str, installed: Installed, action| Part {
        binary: binary.into(),
        installed,
        action,
    };
    let plan = Plan {
        sources: vec![SourcePlan {
            source,
            origin: Origin::Release,
            latest: Ok(Some(Release {
                tag: "v0.2.0".into(),
                version: Version::new(0, 2, 0),
                page: Some("https://github.com/wuphf-rs/wuphf-ai/releases/tag/v0.2.0".into()),
                assets: Vec::new(),
            })),
            parts: vec![
                part("wuphf-server", at("0.1.0"), Action::Update),
                part("wuphf-tui", at("0.2.0"), Action::Current),
                part("wuphf-telegram", Installed::Missing, Action::NotInstalled),
            ],
        }],
    };
    assert_eq!(
        describe(&plan, Path::new("/usr/local/bin")),
        "Binaries in /usr/local/bin:\n\
         wuphf 0.2.0 (https://github.com/wuphf-rs/wuphf-ai/releases/tag/v0.2.0)\n\
         \x20 wuphf-server          0.1.0 → 0.2.0\n\
         \x20 wuphf-tui             0.2.0, current\n"
    );
}
