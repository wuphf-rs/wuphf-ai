# Plan 064: Versions, releases and `wuphf-server update` (every binary, third-party sources, a scheduled check that tells the owner)

> **Executor instructions**: as plan 058. Build limits: one build at a time,
> `CARGO_BUILD_JOBS=2 nice -n 19`, `-p` the crates touched. Stage explicit
> paths only.

## Status

- **Priority**: P1 — the owner asked for it (2026-11-23)
- **Effort**: L
- **Depends on**: — (first, the Go modules moved to `github.com/wuphf-rs/wuphf-ai`)
- **Category**: feature / ops

## Why this matters

Nothing has a version: every crate says `0.1.0`, no binary answers
`--version`, there are no release builds, and installing is copying
`scripts/dev-bin/.bin/wuphf-*` into `/usr/local/bin` by hand. The owner wants
one command that updates every binary, including the parts people build
themselves (their own `wuphf-misc`) and publish on their own git server, and a
scheduled check that tells the owner when an update is out (2026-11-23).

## Design decisions (settled with the owner, 2026-11-23)

1. **One version for the project.** The workspace `version` (and
   whatsapp-rust's, kept equal) is the release; a tag `vX.Y.Z` releases it.
   Every binary prints it: `--version` (Rust, clap) and `--version` /
   `version` (Go, `connectors/shared/version`, stamped with `-ldflags -X`;
   an unstamped build says `dev`). `scripts/release.sh X.Y.Z` bumps,
   commits and tags; CI refuses a tag that disagrees with `Cargo.toml`.
2. **Releases on GitHub** (`github.com/wuphf-rs/wuphf-ai`): a tag runs
   `.github/workflows/release.yml`, which builds the 8 binaries for linux
   `x86_64` and `aarch64` (native runners, ubuntu-22.04: glibc 2.35, the only
   cgo is bundled C) with `scripts/build-release.sh` — the same script a
   source build runs — and publishes `<binary>-linux-<arch>`, `SHA256SUMS` and
   `SHA256SUMS.minisig`.
3. **Signed checksums.** `SHA256SUMS` is signed with minisign. The release
   public key is checked in (`release/minisign.pub`) and compiled in; its
   secret key is the owner's, a GitHub Actions secret. A download is
   installed only when its hash is in a validly signed `SHA256SUMS`. A
   third-party source gives its own key; one without a key is checked
   against its `SHA256SUMS` alone, with a warning, and one with neither is
   refused unless it says `verify = "none"`.
4. **One command updates everything**: `wuphf-server update` (run as root,
   offline from the core) checks every source, shows what changes, asks
   (`--yes` skips), replaces the binaries atomically (keeping the previous
   ones for `--rollback`), then restarts the systemd units that were running
   (`--no-restart` skips). `--check` only reports.
5. **Sources.** The official one (wuphf's 8 binaries) is built in. Others are
   `[[updates.sources]]` in the core's config: a repository on GitHub,
   Gitea/Forgejo (Codeberg) or GitLab, the binaries it provides, its asset
   name pattern (raw binary or `.tar.gz`), its key, its units. A binary a
   source names is that source's, not the official one's: a custom
   `wuphf-misc` is updated from its own releases.
6. **Both release downloads and source builds.** Each source is
   `from = "release"` (default) or `"source"`: clone or fetch the repository,
   check out the release tag, run its build command (the official one:
   `scripts/build-release.sh`), install what it built. `--from-source` forces
   it for one run.
7. **The scheduled check tells the owner.** `wuphf-server serve` checks every
   `updates.check_interval_seconds` (default a day, `0` off) and sends the
   owner one message per new version through the notice targets (the same
   delivery as connector notices), saying what is out, what is installed and
   how to update. It only reads: it never installs.

## Steps

1. Versions: workspace + whatsapp-rust `version`; clap `version` on every
   Rust binary; `connectors/shared/version` and a `version` command in every
   Go binary.
2. `scripts/build-release.sh OUT_DIR` (version-stamped, portable, the 8
   binaries named as installed); `scripts/install-dev-bins.sh` calls it.
3. `release/minisign.pub`, `scripts/release.sh`, `.github/workflows/release.yml`.
4. `crates/wuphf-update`: `config` (the `[updates]` section, described),
   `version` (semver, probing a binary's `--version`), `forge` (GitHub,
   Gitea, GitLab releases), `verify` (SHA256SUMS + minisign), `install`
   (atomic replace, previous copies, rollback, a lock), `source` (git + build
   command), `plan` (installed vs latest, per binary), `services`
   (systemctl), `notice` (the owner's message).
5. `wuphf-server update` (`--check`, `--yes`, `--from-source`, `--only`,
   `--no-restart`, `--rollback`).
6. The scheduled check: `Core::announce` (the notice targets, nothing
   excluded), `OwnerMessageSender::announce`, the task in `serve`.
7. Docs: README "Updates", `deploy/systemd/README.md` (install from a
   release, update), `config/example.toml`.

## Verification

- Unit tests per module: forge JSON parsing (fixtures), asset patterns,
  SHA256SUMS + minisign (a test key), atomic install + rollback in a temp dir,
  plan decisions (newer, same, older, unknown, dev), the notice text, a source
  build from a local git repository.
- `wuphf-server update` end to end against a local HTTP fixture serving a
  signed release (a test key), into a temp install dir.
- Core: an announcement reaches every notice target.
- Every binary's `--version`.
- Live: `--check` against the real GitHub API once releases exist.

## Out of scope

- Installing automatically on a schedule (the check only tells).
- Versions in the connector handshake (a connector reporting its version to
  the core): a follow-up.
- Packages (deb, nix): the flake stays a dev shell.
