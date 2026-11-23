use super::*;
use crate::config::OfficialSource;

fn misc(binaries: &[&str]) -> SourceConfig {
    SourceConfig {
        name: "my-misc".into(),
        repository: "https://codeberg.org/you/wuphf-misc".into(),
        forge: None,
        binaries: binaries.iter().map(|binary| (*binary).to_owned()).collect(),
        asset: "{binary}-{os}-{arch}".into(),
        checksums: "SHA256SUMS".into(),
        public_key: None,
        verify: None,
        from: Origin::Release,
        build: None,
        services: BTreeMap::new(),
        token_env: None,
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn a_binary_another_source_provides_is_not_wuphfs() {
    let config = UpdatesConfig {
        sources: vec![misc(&["wuphf-misc"])],
        ..UpdatesConfig::default()
    };
    let sources = resolve(&config, &no_env).unwrap();
    assert_eq!(sources[0].name, WUPHF);
    assert!(
        !sources[0]
            .binaries
            .iter()
            .any(|binary| binary == "wuphf-misc")
    );
    assert_eq!(sources[0].binaries[0], "wuphf-server");
    assert_eq!(
        sources[0].service("wuphf-server"),
        Some("wuphf-core.service")
    );
    assert_eq!(sources[0].service("wuphf-tui"), None);
    assert_eq!(sources[1].binaries, ["wuphf-misc"]);
    assert_eq!(sources[1].service("wuphf-misc"), Some("wuphf-misc.service"));
    // Without a key, a source's downloads are checked against its sums alone.
    assert_eq!(sources[1].verification, Verification::Checksums);
}

#[test]
fn wuphfs_own_source_can_be_turned_off() {
    let config = UpdatesConfig {
        wuphf: OfficialSource {
            enabled: false,
            ..OfficialSource::default()
        },
        sources: vec![misc(&["wuphf-misc"])],
        ..UpdatesConfig::default()
    };
    let sources = resolve(&config, &no_env).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "my-misc");
}

#[test]
fn a_binary_in_two_sources_is_refused() {
    let mut other = misc(&["wuphf-misc"]);
    other.name = "other".into();
    let config = UpdatesConfig {
        sources: vec![misc(&["wuphf-misc"]), other],
        ..UpdatesConfig::default()
    };
    let error = resolve(&config, &no_env).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("wuphf-misc is in two update sources"),
        "{error}"
    );
}

#[test]
fn a_source_is_checked_as_its_settings_say() {
    let refused = |source: SourceConfig| {
        let config = UpdatesConfig {
            sources: vec![source],
            ..UpdatesConfig::default()
        };
        resolve(&config, &no_env).unwrap_err().to_string()
    };
    assert!(refused(misc(&[])).contains("lists no binaries"));
    assert!(refused(misc(&["../wuphf-misc"])).contains("not a file name"));
    let mut signed = misc(&["wuphf-misc"]);
    signed.verify = Some(Verify::Signed);
    assert!(refused(signed).contains("needs a public_key"));
    let mut built = misc(&["wuphf-misc"]);
    built.from = Origin::Source;
    assert!(refused(built).contains("needs a build command"));
    let mut tokened = misc(&["wuphf-misc"]);
    tokened.token_env = Some("MISC_TOKEN".into());
    assert!(refused(tokened).contains("MISC_TOKEN"));
    let mut unknown_host = misc(&["wuphf-misc"]);
    unknown_host.repository = "https://git.example.org/you/wuphf-misc".into();
    assert!(refused(unknown_host).contains("set its forge"));
}

#[test]
fn an_asset_name_is_filled_in_for_this_host() {
    let mut config = misc(&["wuphf-misc"]);
    config.asset = "{binary}_{version}_{os}_{goarch}.tar.gz".into();
    let source = configured_source(&config, &no_env).unwrap();
    let platform = Platform::current();
    assert_eq!(
        source.asset_name("wuphf-misc", "v1.2.0"),
        format!(
            "wuphf-misc_1.2.0_{}_{}.tar.gz",
            platform.os, platform.goarch
        )
    );
}
