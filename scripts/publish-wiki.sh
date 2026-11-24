#!/usr/bin/env bash
# Write docs/wiki into a checkout of the GitHub wiki (<repo>.wiki.git), with
# the links rewritten for it: a page's `Page.md` link becomes `Page`, and a
# relative link into the repository (`../architecture.md`, `../../README.md`)
# becomes its address on GitHub. Pages the wiki has that docs/wiki no longer
# has are removed. Nothing is committed: the caller does.
#
#   scripts/publish-wiki.sh WIKI_CHECKOUT [REPOSITORY_URL [BRANCH]]
#
# The repository defaults to https://github.com/wuphf-rs/wuphf-ai, the branch
# to main. .github/workflows/wiki.yml runs it on every change to docs/wiki.
set -euo pipefail

readonly REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
readonly WIKI="${1:?usage: $0 WIKI_CHECKOUT [REPOSITORY_URL [BRANCH]]}"
readonly URL="${2:-https://github.com/wuphf-rs/wuphf-ai}"
readonly BRANCH="${3:-main}"
readonly SOURCE="$REPO/docs/wiki"

[ -d "$WIKI/.git" ] || { echo "$WIKI is not a git checkout of the wiki" >&2; exit 1; }

# The wiki is exactly docs/wiki: drop pages that are gone.
find "$WIKI" -maxdepth 1 -name '*.md' -type f | while read -r page; do
    [ -e "$SOURCE/$(basename "$page")" ] || rm -f "$page"
done

for page in "$SOURCE"/*.md; do
    sed -E \
        -e "s#\]\(\.\./\.\./([^)]*)\)#](${URL}/blob/${BRANCH}/\1)#g" \
        -e "s#\]\(\.\./([^)]*)\)#](${URL}/blob/${BRANCH}/docs/\1)#g" \
        -e 's#\]\(([A-Za-z0-9_-]+)\.md(\#[^)]*)?\)#](\1\2)#g' \
        "$page" > "$WIKI/$(basename "$page")"
done
