#!/usr/bin/env bash
# Release wuphf X.Y.Z: set the version (the workspace's and whatsapp-rust's),
# commit, and tag vX.Y.Z. Pushing the tag runs .github/workflows/release.yml,
# which builds every binary and publishes them with signed checksums.
#
#   scripts/release.sh X.Y.Z[-PRERELEASE]
#
# Run on a clean tree inside the nix dev shell. Nothing is pushed: the script
# prints the push command.
set -euo pipefail

readonly REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
readonly VERSION="${1:-}"
readonly SEMVER='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'

if ! [[ "$VERSION" =~ $SEMVER ]]; then
    echo "usage: $0 X.Y.Z[-PRERELEASE]" >&2
    exit 2
fi
cd "$REPO"
if [ -n "$(git status --porcelain)" ]; then
    echo "the tree has changes; commit or stash them first" >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
    echo "v$VERSION is already tagged" >&2
    exit 1
fi

# The first `version = "..."` line: [workspace.package]'s, and [package]'s in
# whatsapp-rust.
set_version() {
    sed -i "0,/^version = \".*\"$/s//version = \"$VERSION\"/" "$1"
}
set_version Cargo.toml
set_version connectors/whatsapp-rust/Cargo.toml
# The lock files record the members' (and whatsapp-rust's path crates')
# versions: resolving rewrites those, and moves nothing else.
cargo metadata --format-version 1 >/dev/null
(cd connectors/whatsapp-rust && cargo metadata --format-version 1 >/dev/null)

git add Cargo.toml Cargo.lock connectors/whatsapp-rust/Cargo.toml connectors/whatsapp-rust/Cargo.lock
git commit -m "release: v$VERSION"
git tag -a "v$VERSION" -m "wuphf v$VERSION"
echo "tagged v$VERSION; publish it with: git push origin HEAD v$VERSION" >&2
