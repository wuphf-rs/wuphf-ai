#!/usr/bin/env bash
# Recompute the vendorHash of every Go package in nix/packages.nix and write
# it, or with --check only say which are stale (exit 1).
#
#   scripts/update-vendor-hashes.sh [--check]
#
# Go vendors the replaced local modules (connectors/shared, proto/go) along
# with the third-party ones, so a change to either, or to any go.mod/go.sum,
# can move every package's hash. Each hash is set to a placeholder first:
# under its old hash Nix would reuse the old vendor tree from the store
# without rebuilding it. Only the fixed-output vendor derivations are built
# (`.#wuphf-<pkg>.goModules`), which fetches modules but compiles nothing.
#
# The flake reads git's view of the tree: stage new files first.
set -euo pipefail

readonly REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
readonly PACKAGES="$REPO/nix/packages.nix"
readonly PLACEHOLDER="sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
check=false
case "${1:-}" in
    --check) check=true ;;
    "") ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

cd "$REPO"
# The packages built with `go { ... }`, in file order.
mapfile -t packages < <(sed -n 's/^  \(wuphf-[a-z-]*\) = go {$/\1/p' "$PACKAGES")
original="$(cat "$PACKAGES")"
restore() { printf '%s\n' "$original" > "$PACKAGES"; }
trap restore EXIT

# The hash a package's `go { ... }` block declares now.
declared() {
    awk -v block="  $1 = go {" '$0 == block {inside=1} inside && /vendorHash = / {
        match($0, /"sha256-[^"]*"/); print substr($0, RSTART + 1, RLENGTH - 2); exit }' "$PACKAGES"
}

# Set a package's hash in nix/packages.nix.
set_hash() {
    local updated
    updated="$(awk -v block="  $1 = go {" -v hash="$2" '$0 == block {inside=1}
        inside && /vendorHash = / {sub(/"sha256-[^"]*"/, "\"" hash "\""); inside=0} {print}' "$PACKAGES")"
    printf '%s\n' "$updated" > "$PACKAGES"
}

declare -A old new
for package in "${packages[@]}"; do
    old[$package]="$(declared "$package")"
    set_hash "$package" "$PLACEHOLDER"
done
for package in "${packages[@]}"; do
    output="$(nix build ".#$package.goModules" --no-link 2>&1 || true)"
    got="$(printf '%s\n' "$output" | sed -n 's/^ *got: *\(sha256-[^ ]*\)$/\1/p' | head -1)"
    if [ -z "$got" ]; then
        printf '%s\n' "$output" | tail -20 >&2
        echo "$package: could not compute its vendor hash" >&2
        exit 1
    fi
    new[$package]="$got"
done

restore
stale=()
for package in "${packages[@]}"; do
    if [ "${old[$package]}" != "${new[$package]}" ]; then
        stale+=("$package")
        $check || set_hash "$package" "${new[$package]}"
    fi
done
if $check; then
    trap - EXIT
    if [ ${#stale[@]} -gt 0 ]; then
        echo "stale vendorHash in nix/packages.nix: ${stale[*]}" >&2
        echo "run scripts/update-vendor-hashes.sh and stage nix/packages.nix" >&2
        exit 1
    fi
    echo "vendor hashes are current"
    exit 0
fi
trap - EXIT
if [ ${#stale[@]} -gt 0 ]; then
    echo "updated: ${stale[*]}"
else
    echo "vendor hashes are current"
fi
