//! An update end to end: releases served from fixtures, signed with a key made
//! here, installed into a temp directory, units recorded.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::config::{OfficialSource, SourceConfig};
use crate::plan::Action;
use crate::source::Platform;

/// A request: its URL and headers.
type Request = (String, Vec<(String, String)>);

/// Serves fixed bodies by URL, recording each request's headers.
#[derive(Default)]
struct Fixtures {
    bodies: HashMap<String, Vec<u8>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl Fetch for Fixtures {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        _limit: u64,
    ) -> Result<Vec<u8>, Error> {
        self.requests
            .lock()
            .unwrap()
            .push((url.to_owned(), headers.to_vec()));
        self.bodies
            .get(url)
            .cloned()
            .ok_or_else(|| Error::Http(format!("GET {url}: 404 Not Found")))
    }
}

/// Units by whether they run; records restarts.
struct Units {
    active: Vec<&'static str>,
    restarted: Mutex<Vec<String>>,
}

#[async_trait]
impl Services for Units {
    async fn is_active(&self, unit: &str) -> bool {
        self.active.contains(&unit)
    }

    async fn restart(&self, unit: &str) -> Result<(), Error> {
        self.restarted.lock().unwrap().push(unit.to_owned());
        Ok(())
    }
}

fn units(active: &[&'static str]) -> Units {
    Units {
        active: active.to_vec(),
        restarted: Mutex::default(),
    }
}

/// A binary that says it is `name version`.
fn binary(name: &str, version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"{name} {version}\"\n").into_bytes()
}

fn install(dir: &Path, name: &str, contents: &[u8]) {
    std::fs::write(dir.join(name), contents).unwrap();
    install::make_executable(&dir.join(name)).unwrap();
}

fn arch() -> &'static str {
    Platform::current().arch
}

struct Key {
    public: String,
    pair: minisign::KeyPair,
}

fn key() -> Key {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    Key {
        public: pair.pk.to_box().unwrap().to_string(),
        pair,
    }
}

fn sign(key: &Key, data: &[u8], comment: &str) -> Vec<u8> {
    minisign::sign(
        Some(&key.pair.pk),
        &key.pair.sk,
        Cursor::new(data),
        Some(comment),
        None,
    )
    .unwrap()
    .to_string()
    .into_bytes()
}

const WUPHF_RELEASES: &str = "https://api.github.com/repos/wuphf-rs/wuphf-ai/releases?per_page=30";
const WUPHF_DOWNLOADS: &str = "https://github.com/wuphf-rs/wuphf-ai/releases/download/v0.2.0";
const MISC_RELEASES_API: &str =
    "https://codeberg.org/api/v1/repos/you/wuphf-misc/releases?limit=30";
const MISC_DOWNLOADS: &str = "https://codeberg.org/you/wuphf-misc/releases/download/v1.1.0";

/// wuphf v0.2.0 (the server and the Telegram connector), signed by `key` with
/// `comment`; my-misc v1.1.0, a `.tar.gz` with unsigned checksums.
fn releases(key: &Key, comment: &str) -> Fixtures {
    let mut fixtures = Fixtures::default();
    let mut sums = String::new();
    let mut wuphf_assets = Vec::new();
    for name in ["wuphf-server", "wuphf-telegram"] {
        let asset = format!("{name}-linux-{}", arch());
        let contents = binary(name, "0.2.0");
        sums.push_str(&format!("{}  {asset}\n", verify::sha256_hex(&contents)));
        wuphf_assets.push(
            json!({"name": asset, "browser_download_url": format!("{WUPHF_DOWNLOADS}/{asset}")}),
        );
        fixtures
            .bodies
            .insert(format!("{WUPHF_DOWNLOADS}/{asset}"), contents);
    }
    for name in ["SHA256SUMS", "SHA256SUMS.minisig"] {
        wuphf_assets.push(
            json!({"name": name, "browser_download_url": format!("{WUPHF_DOWNLOADS}/{name}")}),
        );
    }
    fixtures.bodies.insert(
        format!("{WUPHF_DOWNLOADS}/SHA256SUMS.minisig"),
        sign(key, sums.as_bytes(), comment),
    );
    fixtures
        .bodies
        .insert(format!("{WUPHF_DOWNLOADS}/SHA256SUMS"), sums.into_bytes());
    fixtures.bodies.insert(
        WUPHF_RELEASES.into(),
        serde_json::to_vec(&json!([
            {"tag_name": "v0.3.0-rc.1", "prerelease": true, "assets": []},
            {"tag_name": "v0.2.0", "html_url": "https://github.com/wuphf-rs/wuphf-ai/releases/tag/v0.2.0", "assets": wuphf_assets},
            {"tag_name": "v0.1.0", "assets": []},
        ]))
        .unwrap(),
    );

    let archive = format!(
        "wuphf-misc-1.1.0-linux-{}.tar.gz",
        Platform::current().goarch
    );
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let contents = binary("wuphf-misc", "1.1.0");
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "wuphf-misc", contents.as_slice())
        .unwrap();
    let archive_bytes = builder.into_inner().unwrap().finish().unwrap();
    fixtures.bodies.insert(
        format!("{MISC_DOWNLOADS}/SHA256SUMS"),
        format!("{}  {archive}\n", verify::sha256_hex(&archive_bytes)).into_bytes(),
    );
    fixtures
        .bodies
        .insert(format!("{MISC_DOWNLOADS}/{archive}"), archive_bytes);
    fixtures.bodies.insert(
        MISC_RELEASES_API.into(),
        serde_json::to_vec(&json!([{"tag_name": "v1.1.0", "assets": [
            {"name": archive, "browser_download_url": format!("{MISC_DOWNLOADS}/{archive}")},
            {"name": "SHA256SUMS", "browser_download_url": format!("{MISC_DOWNLOADS}/SHA256SUMS")},
        ]}]))
        .unwrap(),
    );
    fixtures
}

fn config(dir: &Path, key: &Key) -> UpdatesConfig {
    UpdatesConfig {
        install_dir: Some(dir.join("bin")),
        state_dir: dir.join("state"),
        wuphf: OfficialSource {
            public_key: Some(key.public.clone()),
            ..OfficialSource::default()
        },
        sources: vec![SourceConfig {
            name: "my-misc".into(),
            repository: "https://codeberg.org/you/wuphf-misc".into(),
            forge: None,
            binaries: vec!["wuphf-misc".into()],
            asset: "wuphf-misc-{version}-{os}-{goarch}.tar.gz".into(),
            checksums: "SHA256SUMS".into(),
            public_key: None,
            verify: None,
            from: Origin::Release,
            build: None,
            services: Default::default(),
            token_env: Some("MISC_TOKEN".into()),
        }],
        ..UpdatesConfig::default()
    }
}

/// An install directory with the server, Telegram and misc at their old
/// versions, and an updater over `fixtures`.
fn setup(key: &Key, fixtures: Fixtures) -> (tempfile::TempDir, Updater, Arc<Fixtures>) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    install(&bin, "wuphf-server", &binary("wuphf-server", "0.1.0"));
    install(&bin, "wuphf-telegram", &binary("wuphf-telegram", "0.1.0"));
    install(&bin, "wuphf-misc", &binary("wuphf-misc", "1.0.0"));
    let fixtures = Arc::new(fixtures);
    let env = |name: &str| (name == "MISC_TOKEN").then(|| "misc-secret".to_owned());
    let updater = Updater::new(&config(dir.path(), key), &env, fixtures.clone()).unwrap();
    (dir, updater, fixtures)
}

fn said(dir: &Path, name: &str) -> String {
    String::from_utf8(std::fs::read(dir.join("bin").join(name)).unwrap()).unwrap()
}

#[tokio::test]
async fn every_outdated_binary_is_updated_and_the_running_units_restarted() {
    let key = key();
    let (dir, updater, fixtures) = setup(&key, releases(&key, "wuphf v0.2.0"));

    let plan = updater.plan(&PlanOptions::default()).await.unwrap();
    let actions: Vec<_> = plan
        .sources
        .iter()
        .flat_map(|source| {
            source
                .parts
                .iter()
                .map(|part| (part.binary.as_str(), part.action))
        })
        .filter(|(_, action)| *action != Action::NotInstalled)
        .collect();
    assert_eq!(
        actions,
        [
            ("wuphf-server", Action::Update),
            ("wuphf-telegram", Action::Update),
            ("wuphf-misc", Action::Update),
        ]
    );
    // The custom source owns wuphf-misc: wuphf's own does not list it.
    assert!(
        !plan.sources[0]
            .parts
            .iter()
            .any(|part| part.binary == "wuphf-misc")
    );

    let units = units(&["wuphf-core.service", "wuphf-misc.service"]);
    let applied = updater.apply(&plan, Some(&units), &|_| {}).await.unwrap();
    assert_eq!(applied.installed.len(), 3);
    assert!(said(dir.path(), "wuphf-server").contains("wuphf-server 0.2.0"));
    assert!(said(dir.path(), "wuphf-telegram").contains("wuphf-telegram 0.2.0"));
    assert!(said(dir.path(), "wuphf-misc").contains("wuphf-misc 1.1.0"));
    // Only what ran is restarted, the core first.
    assert_eq!(
        *units.restarted.lock().unwrap(),
        ["wuphf-core.service", "wuphf-misc.service"]
    );

    // The token went to the custom source's forge only.
    for (url, headers) in fixtures.requests.lock().unwrap().iter() {
        let sent_token = headers
            .iter()
            .any(|(_, value)| value.contains("misc-secret"));
        assert_eq!(
            sent_token,
            url.starts_with("https://codeberg.org/"),
            "{url}"
        );
    }

    // Everything is current now.
    let plan = updater.plan(&PlanOptions::default()).await.unwrap();
    assert!(!plan.has_updates());

    // And the rollback puts the old ones back.
    let rolled = updater.rollback(Some(&units), &|_| {}).await.unwrap();
    assert_eq!(rolled.installed.len(), 3);
    assert!(said(dir.path(), "wuphf-server").contains("wuphf-server 0.1.0"));
    assert!(said(dir.path(), "wuphf-misc").contains("wuphf-misc 1.0.0"));
}

#[tokio::test]
async fn only_the_named_binaries_are_updated() {
    let key = key();
    let (dir, updater, _) = setup(&key, releases(&key, "wuphf v0.2.0"));
    let plan = updater
        .plan(&PlanOptions {
            only: vec!["wuphf-telegram".into()],
            ..PlanOptions::default()
        })
        .await
        .unwrap();
    updater.apply(&plan, None, &|_| {}).await.unwrap();
    assert!(said(dir.path(), "wuphf-telegram").contains("0.2.0"));
    assert!(said(dir.path(), "wuphf-server").contains("0.1.0"));
    assert!(said(dir.path(), "wuphf-misc").contains("1.0.0"));

    let unknown = updater
        .plan(&PlanOptions {
            only: vec!["wuphf-nothing".into()],
            ..PlanOptions::default()
        })
        .await
        .unwrap_err();
    assert!(
        unknown
            .to_string()
            .contains("no update source provides wuphf-nothing")
    );
}

#[tokio::test]
async fn a_signature_by_another_key_installs_nothing() {
    let key = key();
    let (dir, updater, _) = setup(&key, releases(&self::key(), "wuphf v0.2.0"));
    let plan = updater.plan(&PlanOptions::default()).await.unwrap();
    let error = updater.apply(&plan, None, &|_| {}).await.unwrap_err();
    assert!(error.to_string().contains("does not verify"), "{error}");
    assert!(said(dir.path(), "wuphf-server").contains("0.1.0"));
    assert!(said(dir.path(), "wuphf-misc").contains("1.0.0"));
}

#[tokio::test]
async fn an_older_releases_signature_does_not_pass_for_a_newer_one() {
    let key = key();
    let (dir, updater, _) = setup(&key, releases(&key, "wuphf v0.1.0"));
    let plan = updater.plan(&PlanOptions::default()).await.unwrap();
    let error = updater.apply(&plan, None, &|_| {}).await.unwrap_err();
    assert!(
        error.to_string().contains("not \"wuphf v0.2.0\""),
        "{error}"
    );
    assert!(said(dir.path(), "wuphf-telegram").contains("0.1.0"));
}

#[tokio::test]
async fn a_download_whose_sum_differs_installs_nothing() {
    let key = key();
    let mut fixtures = releases(&key, "wuphf v0.2.0");
    let asset = format!("{WUPHF_DOWNLOADS}/wuphf-telegram-linux-{}", arch());
    fixtures
        .bodies
        .insert(asset, binary("wuphf-telegram", "6.6.6"));
    let (dir, updater, _) = setup(&key, fixtures);
    let plan = updater.plan(&PlanOptions::default()).await.unwrap();
    let error = updater.apply(&plan, None, &|_| {}).await.unwrap_err();
    assert!(error.to_string().contains("SHA-256"), "{error}");
    assert!(said(dir.path(), "wuphf-server").contains("0.1.0"));
}

#[tokio::test]
async fn a_source_that_cannot_be_asked_does_not_stop_the_others() {
    let key = key();
    let mut fixtures = releases(&key, "wuphf v0.2.0");
    fixtures.bodies.remove(MISC_RELEASES_API);
    let (_dir, updater, _) = setup(&key, fixtures);
    let plan = updater.plan(&PlanOptions::default()).await.unwrap();
    assert!(plan.sources[0].release().is_some());
    let misc = &plan.sources[1];
    assert!(
        misc.latest.as_ref().unwrap_err().contains("404"),
        "{:?}",
        misc.latest
    );
    assert_eq!(misc.parts[0].action, Action::NoRelease);
}

#[tokio::test]
async fn a_named_binary_is_installed_even_when_it_is_not_yet() {
    let key = key();
    let (dir, updater, _) = setup(&key, releases(&key, "wuphf v0.2.0"));
    std::fs::remove_file(dir.path().join("bin/wuphf-telegram")).unwrap();

    // Not named, a missing binary stays missing.
    let plan = updater.plan(&PlanOptions::default()).await.unwrap();
    let telegram = plan.sources[0]
        .parts
        .iter()
        .find(|part| part.binary == "wuphf-telegram")
        .unwrap();
    assert_eq!(telegram.action, Action::NotInstalled);

    let plan = updater
        .plan(&PlanOptions {
            only: vec!["wuphf-telegram".into()],
            ..PlanOptions::default()
        })
        .await
        .unwrap();
    updater.apply(&plan, None, &|_| {}).await.unwrap();
    assert!(said(dir.path(), "wuphf-telegram").contains("wuphf-telegram 0.2.0"));
}
