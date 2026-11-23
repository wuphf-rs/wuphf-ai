//! The `[updates]` section of the core's configuration: where the binaries
//! are installed, where their releases come from, and how often `serve`
//! checks for new ones.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// wuphf's own repository.
pub const WUPHF_REPOSITORY: &str = "https://github.com/wuphf-rs/wuphf-ai";

/// Updates: `wuphf-server update` replaces every installed binary with its
/// latest release, and `serve` checks for new releases and tells the owner.
///
/// wuphf's own binaries come from wuphf's releases (`[updates.wuphf]`);
/// binaries built elsewhere (your own `wuphf-misc`, a connector of your own)
/// come from their own (`[[updates.sources]]`). Every download is checked
/// against its release's SHA256SUMS, signed with minisign.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(default)]
pub struct UpdatesConfig {
    /// How often `serve` checks for new releases, in seconds; `0` never
    /// checks. Each new release is told to the owner once, through the notice
    /// chats (`notify`); the check never installs anything.
    pub check_interval_seconds: u64,
    /// The directory the binaries are installed in. Unset: the directory of
    /// the running `wuphf-server`.
    pub install_dir: Option<PathBuf>,
    /// Where `wuphf-server update` keeps the binaries it replaced (for
    /// `--rollback`), its source checkouts and its lock.
    pub state_dir: PathBuf,
    /// Pre-releases (`v1.2.0-rc.1`) count as updates.
    pub prerelease: bool,
    /// wuphf's own binaries.
    pub wuphf: OfficialSource,
    /// Other release sources, each providing some binaries. A binary a
    /// source lists is updated from that source only, never from wuphf's
    /// releases: list your own `wuphf-misc` here to keep it.
    pub sources: Vec<SourceConfig>,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            check_interval_seconds: 24 * 60 * 60,
            install_dir: None,
            state_dir: PathBuf::from("/var/lib/wuphf/update"),
            prerelease: false,
            wuphf: OfficialSource::default(),
            sources: Vec::new(),
        }
    }
}

/// wuphf's own binaries: the core, the TUI, the connectors and providers.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(default)]
pub struct OfficialSource {
    /// Update wuphf's own binaries. Off: only `sources` are updated and
    /// checked.
    pub enabled: bool,
    /// `release` installs the release's downloads; `source` builds the
    /// release's tag (needs git, cargo, go and a C toolchain on this host).
    pub from: Origin,
    /// The repository: wuphf's own, or a fork publishing releases the same way
    /// (set `public_key` to the fork's key).
    pub repository: String,
    /// The minisign public key the release's SHA256SUMS is signed with: the
    /// `.pub` file, or its key line. Unset: wuphf's own release key.
    pub public_key: Option<String>,
    /// Environment variable holding a token for the repository's API, for a
    /// private fork or past GitHub's 60 unauthenticated requests an hour.
    #[cfg_attr(feature = "describe", botconf(env = token))]
    pub token_env: Option<String>,
}

impl Default for OfficialSource {
    fn default() -> Self {
        Self {
            enabled: true,
            from: Origin::Release,
            repository: WUPHF_REPOSITORY.to_owned(),
            public_key: None,
            token_env: None,
        }
    }
}

/// A release source of its own: a repository on GitHub, Gitea, Forgejo
/// (Codeberg) or GitLab whose releases carry some binaries.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
pub struct SourceConfig {
    /// The source's name, in messages and in the owner's update notice.
    pub name: String,
    /// The repository's web address (`https://codeberg.org/you/wuphf-misc`).
    pub repository: String,
    /// The forge's API. Unset: told by the host (github.com, gitlab.com,
    /// codeberg.org); required for any other host.
    #[serde(default)]
    pub forge: Option<Forge>,
    /// The binaries the source provides, by installed file name.
    pub binaries: Vec<String>,
    /// The release asset holding a binary. `{binary}`, `{tag}`, `{version}`
    /// (the tag without its `v`), `{os}` (`linux`), `{arch}` (`x86_64`,
    /// `aarch64`) and `{goarch}` (`amd64`, `arm64`) are filled in. A name
    /// ending in `.tar.gz` or `.tgz` is an archive holding a file named as
    /// the binary.
    #[serde(default = "default_asset")]
    pub asset: String,
    /// The release asset listing the assets' SHA-256 sums (`sha256sum`'s
    /// format). Its signature is the same name plus `.minisig`.
    #[serde(default = "default_checksums")]
    pub checksums: String,
    /// The minisign public key the checksums are signed with: the `.pub`
    /// file, or its key line.
    #[serde(default)]
    pub public_key: Option<String>,
    /// What a download must pass: `signed` (a valid signature on the
    /// checksums, then its sum), `checksums` (its sum alone), or `none`.
    /// Unset: `signed` with a `public_key`, `checksums` without.
    #[serde(default)]
    pub verify: Option<Verify>,
    /// `release` installs the release's downloads; `source` clones the
    /// repository, checks out the newest version tag and runs `build`.
    #[serde(default)]
    pub from: Origin,
    /// The build for `from = "source"`: run with `sh -c` in the checkout, it
    /// puts the binaries, named as installed, in the directory `$OUT`.
    #[serde(default)]
    pub build: Option<String>,
    /// The systemd unit each binary runs as, by binary: restarted after an
    /// update when it was running. A binary not listed: `<binary>.service`.
    #[serde(default)]
    pub services: BTreeMap<String, String>,
    /// Environment variable holding a token for the forge's API (a private
    /// repository).
    #[serde(default)]
    #[cfg_attr(feature = "describe", botconf(env = token))]
    pub token_env: Option<String>,
}

fn default_asset() -> String {
    "{binary}-{os}-{arch}".to_owned()
}

fn default_checksums() -> String {
    "SHA256SUMS".to_owned()
}

/// Where a source's binaries come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// The release's downloads.
    #[default]
    Release,
    /// A build of the release's tag.
    Source,
}

/// A forge's release API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(rename_all = "lowercase")]
pub enum Forge {
    /// GitHub, or GitHub Enterprise (`/api/v3`).
    Github,
    /// Gitea or Forgejo (Codeberg).
    Gitea,
    /// GitLab.
    Gitlab,
}

/// What a download must pass before it is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(rename_all = "lowercase")]
pub enum Verify {
    /// A valid signature on the checksums, then its sum.
    Signed,
    /// Its sum in the checksums.
    Checksums,
    /// Nothing.
    None,
}
