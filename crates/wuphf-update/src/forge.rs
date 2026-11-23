//! Forges' release APIs (GitHub, Gitea/Forgejo, GitLab), and a repository's
//! version tags for source builds.

use semver::Version;
use serde::Deserialize;

use crate::Error;
use crate::config::Forge;
use crate::http::Fetch;
use crate::version::parse_tag;

/// How many of a repository's newest releases are looked at.
const RELEASES_PAGE: usize = 30;
/// A release listing larger than this is not a release listing.
const LISTING_LIMIT: u64 = 8 * 1024 * 1024;

/// A repository on a forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub forge: Forge,
    /// `https://host`.
    pub origin: String,
    /// `owner/name` (GitLab: `group/subgroup/name`).
    pub path: String,
}

impl Repository {
    /// A repository from its web address, the forge told by `forge` or, unset,
    /// by the host.
    pub fn parse(address: &str, forge: Option<Forge>) -> Result<Self, Error> {
        let invalid = |why: &str| Error::Config(format!("repository {address:?}: {why}"));
        let rest = address
            .strip_prefix("https://")
            .or_else(|| address.strip_prefix("http://"))
            .ok_or_else(|| invalid("not an http(s) address"))?;
        let (host, path) = rest
            .split_once('/')
            .ok_or_else(|| invalid("no owner/name"))?;
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        if host.is_empty() || path.split('/').filter(|part| !part.is_empty()).count() < 2 {
            return Err(invalid("no owner/name"));
        }
        let forge = match forge {
            Some(forge) => forge,
            None => match host {
                "github.com" => Forge::Github,
                "gitlab.com" => Forge::Gitlab,
                "codeberg.org" => Forge::Gitea,
                _ => return Err(invalid("set its forge (github, gitea or gitlab)")),
            },
        };
        let scheme = if address.starts_with("http://") {
            "http"
        } else {
            "https"
        };
        Ok(Self {
            forge,
            origin: format!("{scheme}://{host}"),
            path: path.to_owned(),
        })
    }

    /// The repository's web address, which git clones.
    pub fn web(&self) -> String {
        format!("{}/{}", self.origin, self.path)
    }

    /// The API listing the newest releases.
    fn releases_url(&self) -> String {
        match self.forge {
            Forge::Github if self.origin == "https://github.com" => format!(
                "https://api.github.com/repos/{}/releases?per_page={RELEASES_PAGE}",
                self.path
            ),
            Forge::Github => format!(
                "{}/api/v3/repos/{}/releases?per_page={RELEASES_PAGE}",
                self.origin, self.path
            ),
            Forge::Gitea => format!(
                "{}/api/v1/repos/{}/releases?limit={RELEASES_PAGE}",
                self.origin, self.path
            ),
            Forge::Gitlab => format!(
                "{}/api/v4/projects/{}/releases?per_page={RELEASES_PAGE}",
                self.origin,
                self.path.replace('/', "%2F")
            ),
        }
    }

    /// The headers a request for `url` carries: the token, the way the forge
    /// wants it, but only to the forge itself (an asset link may point
    /// anywhere).
    pub fn headers(&self, url: &str, token: Option<&str>) -> Vec<(String, String)> {
        let ours = url.starts_with(&format!("{}/", self.origin))
            || (self.forge == Forge::Github && url.starts_with("https://api.github.com/"));
        match token {
            Some(token) if ours => vec![match self.forge {
                Forge::Github => ("authorization".into(), format!("Bearer {token}")),
                Forge::Gitea => ("authorization".into(), format!("token {token}")),
                Forge::Gitlab => ("private-token".into(), token.to_owned()),
            }],
            _ => Vec::new(),
        }
    }

    /// The headers an API request carries: [`Repository::headers`] and the
    /// API's media type.
    fn api_headers(&self, url: &str, token: Option<&str>) -> Vec<(String, String)> {
        let mut headers = self.headers(url, token);
        if self.forge == Forge::Github {
            headers.push(("accept".into(), "application/vnd.github+json".into()));
        }
        headers
    }
}

/// A release: its tag, version and downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    /// The release's web page.
    pub page: Option<String>,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// The repository's newest releases (drafts left out, as are tags that are not
/// versions).
pub async fn releases(
    http: &dyn Fetch,
    repository: &Repository,
    token: Option<&str>,
) -> Result<Vec<Release>, Error> {
    let url = repository.releases_url();
    let body = http
        .get(&url, &repository.api_headers(&url, token), LISTING_LIMIT)
        .await?;
    parse_releases(repository.forge, &body)
}

/// A release listing, as `forge` writes one.
pub fn parse_releases(forge: Forge, body: &[u8]) -> Result<Vec<Release>, Error> {
    let invalid =
        |error: serde_json::Error| Error::Forge(format!("unexpected release listing: {error}"));
    let releases = match forge {
        // Gitea and Forgejo copied GitHub's shape.
        Forge::Github | Forge::Gitea => serde_json::from_slice::<Vec<GithubRelease>>(body)
            .map_err(invalid)?
            .into_iter()
            .filter(|release| !release.draft)
            .filter_map(|release| {
                Some(Release {
                    version: parse_tag(&release.tag_name)?,
                    tag: release.tag_name,
                    page: release.html_url,
                    assets: release
                        .assets
                        .into_iter()
                        .map(|asset| Asset {
                            name: asset.name,
                            url: asset.browser_download_url,
                        })
                        .collect(),
                })
            })
            .collect(),
        Forge::Gitlab => serde_json::from_slice::<Vec<GitlabRelease>>(body)
            .map_err(invalid)?
            .into_iter()
            .filter(|release| !release.upcoming_release)
            .filter_map(|release| {
                Some(Release {
                    version: parse_tag(&release.tag_name)?,
                    tag: release.tag_name,
                    page: release.links.and_then(|links| links.self_),
                    assets: release
                        .assets
                        .links
                        .into_iter()
                        .map(|link| Asset {
                            name: link.name,
                            url: link.direct_asset_url.unwrap_or(link.url),
                        })
                        .collect(),
                })
            })
            .collect(),
    };
    Ok(releases)
}

/// The newest release; a pre-release only when `prerelease`.
pub fn latest(releases: Vec<Release>, prerelease: bool) -> Option<Release> {
    releases
        .into_iter()
        .filter(|release| prerelease || release.version.pre.is_empty())
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// The repository's version tags (`git ls-remote`), as releases without
/// downloads: what a source build checks out.
pub async fn tags(repository: &Repository) -> Result<Vec<Release>, Error> {
    let output = tokio::process::Command::new("git")
        .args(["ls-remote", "--tags", "--refs", "--", &repository.web()])
        // Never wait on a credentials prompt (the scheduled check has no terminal).
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| Error::Source(format!("run git: {error}")))?;
    if !output.status.success() {
        return Err(Error::Source(format!(
            "git ls-remote {}: {}",
            repository.web(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(parse_tags(&String::from_utf8_lossy(&output.stdout)))
}

/// `git ls-remote --tags --refs` output, as releases.
pub fn parse_tags(output: &str) -> Vec<Release> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1)?.strip_prefix("refs/tags/"))
        .filter_map(|tag| {
            Some(Release {
                version: parse_tag(tag)?,
                tag: tag.to_owned(),
                page: None,
                assets: Vec::new(),
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GitlabRelease {
    tag_name: String,
    #[serde(default)]
    upcoming_release: bool,
    #[serde(default, rename = "_links")]
    links: Option<GitlabLinks>,
    #[serde(default)]
    assets: GitlabAssets,
}

#[derive(Deserialize)]
struct GitlabLinks {
    #[serde(default, rename = "self")]
    self_: Option<String>,
}

#[derive(Deserialize, Default)]
struct GitlabAssets {
    #[serde(default)]
    links: Vec<GitlabLink>,
}

#[derive(Deserialize)]
struct GitlabLink {
    name: String,
    url: String,
    #[serde(default)]
    direct_asset_url: Option<String>,
}

#[cfg(test)]
mod tests;
