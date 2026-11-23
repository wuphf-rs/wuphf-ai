use super::*;

#[test]
fn a_repository_is_read_from_its_web_address() {
    let github = Repository::parse("https://github.com/wuphf-rs/wuphf-ai.git/", None).unwrap();
    assert_eq!(github.forge, Forge::Github);
    assert_eq!(github.path, "wuphf-rs/wuphf-ai");
    assert_eq!(
        github.releases_url(),
        "https://api.github.com/repos/wuphf-rs/wuphf-ai/releases?per_page=30"
    );
    let gitlab = Repository::parse("https://gitlab.com/group/sub/tool", None).unwrap();
    assert_eq!(
        gitlab.releases_url(),
        "https://gitlab.com/api/v4/projects/group%2Fsub%2Ftool/releases?per_page=30"
    );
    let forgejo =
        Repository::parse("https://git.example.org/you/misc", Some(Forge::Gitea)).unwrap();
    assert_eq!(
        forgejo.releases_url(),
        "https://git.example.org/api/v1/repos/you/misc/releases?limit=30"
    );
    let enterprise =
        Repository::parse("https://ghe.example.com/team/misc", Some(Forge::Github)).unwrap();
    assert_eq!(
        enterprise.releases_url(),
        "https://ghe.example.com/api/v3/repos/team/misc/releases?per_page=30"
    );
    assert!(Repository::parse("git@github.com:wuphf-rs/wuphf-ai", None).is_err());
    assert!(Repository::parse("https://github.com/wuphf", None).is_err());
}

#[test]
fn the_token_goes_to_the_forge_only() {
    let github = Repository::parse("https://github.com/wuphf-rs/wuphf-ai", None).unwrap();
    let api = "https://api.github.com/repos/wuphf-rs/wuphf-ai/releases";
    assert_eq!(
        github.api_headers(api, Some("t")),
        [
            ("authorization".to_owned(), "Bearer t".to_owned()),
            (
                "accept".to_owned(),
                "application/vnd.github+json".to_owned()
            )
        ]
    );
    assert!(
        github
            .headers("https://example.com/asset", Some("t"))
            .is_empty()
    );
    let gitlab = Repository::parse("https://gitlab.com/you/misc", None).unwrap();
    assert_eq!(
        gitlab.headers(
            "https://gitlab.com/you/misc/-/releases/v1/downloads/x",
            Some("t")
        ),
        [("private-token".to_owned(), "t".to_owned())]
    );
    // A host that only starts like the forge's is not the forge.
    assert!(
        gitlab
            .headers("https://gitlab.com.evil.example/x", Some("t"))
            .is_empty()
    );
}

#[test]
fn github_and_gitea_releases_are_read_drafts_and_non_versions_left_out() {
    let body = br#"[
        {"tag_name": "v1.1.0", "draft": true, "assets": []},
        {"tag_name": "nightly", "assets": []},
        {"tag_name": "v1.0.0", "html_url": "https://github.com/o/r/releases/tag/v1.0.0",
         "assets": [{"name": "SHA256SUMS", "browser_download_url": "https://github.com/o/r/d/SHA256SUMS"}]}
    ]"#;
    for forge in [Forge::Github, Forge::Gitea] {
        let releases = parse_releases(forge, body).unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].tag, "v1.0.0");
        assert_eq!(
            releases[0].asset("SHA256SUMS").unwrap().url,
            "https://github.com/o/r/d/SHA256SUMS"
        );
    }
}

#[test]
fn gitlab_releases_are_read_from_their_asset_links() {
    let body = br#"[
        {"tag_name": "v2.0.0", "upcoming_release": true},
        {"tag_name": "v1.4.0", "_links": {"self": "https://gitlab.com/o/r/-/releases/v1.4.0"},
         "assets": {"links": [
            {"name": "tool-linux-x86_64", "url": "https://gitlab.com/o/r/-/raw/x",
             "direct_asset_url": "https://gitlab.com/o/r/-/releases/v1.4.0/downloads/tool-linux-x86_64"}
         ]}}
    ]"#;
    let releases = parse_releases(Forge::Gitlab, body).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].page.as_deref(),
        Some("https://gitlab.com/o/r/-/releases/v1.4.0")
    );
    assert_eq!(
        releases[0].assets[0].url,
        "https://gitlab.com/o/r/-/releases/v1.4.0/downloads/tool-linux-x86_64"
    );
}

#[test]
fn the_newest_release_is_a_pre_release_only_when_asked() {
    let releases = parse_tags(
        "a1\trefs/tags/v1.2.0\nb2\trefs/tags/v1.10.0\nc3\trefs/tags/v2.0.0-rc.1\nd4\trefs/tags/latest\n",
    );
    assert_eq!(releases.len(), 3);
    assert_eq!(latest(releases.clone(), false).unwrap().tag, "v1.10.0");
    assert_eq!(latest(releases, true).unwrap().tag, "v2.0.0-rc.1");
    assert_eq!(latest(Vec::new(), true), None);
}

#[test]
fn a_github_asset_downloads_through_the_api_with_a_token() {
    let body = br#"[{"tag_name": "v1.0.0", "assets": [{
        "name": "wuphf-server-linux-x86_64",
        "url": "https://api.github.com/repos/o/r/releases/assets/42",
        "browser_download_url": "https://github.com/o/r/releases/download/v1.0.0/wuphf-server-linux-x86_64"
    }]}]"#;
    let asset = &parse_releases(Forge::Github, body).unwrap()[0].assets[0];
    let github = Repository::parse("https://github.com/o/r", None).unwrap();

    // Public: the download link, no token needed.
    let (url, headers) = github.asset_request(asset, None);
    assert_eq!(url, asset.url);
    assert!(headers.is_empty());

    // Private: only the API serves it to a token, as raw bytes.
    let (url, headers) = github.asset_request(asset, Some("t"));
    assert_eq!(url, "https://api.github.com/repos/o/r/releases/assets/42");
    assert_eq!(
        headers,
        [
            ("authorization".to_owned(), "Bearer t".to_owned()),
            ("accept".to_owned(), "application/octet-stream".to_owned())
        ]
    );
}
