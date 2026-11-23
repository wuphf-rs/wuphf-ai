#!/usr/bin/env bash
# Build wuphf's release binaries into OUT_DIR, each named as it is installed
# (wuphf-server, wuphf-tui, wuphf-whatsapp-rust, wuphf-telegram,
# wuphf-whatsapp, wuphf-instagram, wuphf-mediaconv, wuphf-misc), every one
# stamped with the release version (`<binary> --version`).
#
#   scripts/build-release.sh [--target TRIPLE] OUT_DIR [BINARY...]
#
# With BINARY arguments only those are built. --target cross-compiles for a
# Rust target triple (`aarch64-unknown-linux-gnu`): the Rust builds get
# --target, the Go builds the matching GOOS/GOARCH, and the cgo ones the C and
# C++ compilers in CC_<triple> and CXX_<triple> (dashes as underscores, as
# cargo's cc crate reads them; the linker is CARGO_TARGET_<TRIPLE>_LINKER). The release workflow, a source
# update (`wuphf-server update --from-source`) and scripts/install-dev-bins.sh
# all build through here.
#
# Needs cargo, go and a C toolchain (cgo: wuphf-whatsapp's SQLite and
# wuphf-mediaconv's image codecs; the other Go binaries build without one, as
# static binaries). The version is the workspace's (Cargo.toml); whatsapp-rust's
# must equal it.
set -euo pipefail

readonly REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
readonly ALL=(wuphf-server wuphf-tui wuphf-whatsapp-rust wuphf-telegram wuphf-whatsapp wuphf-instagram wuphf-mediaconv wuphf-misc)

TARGET=""
if [ "${1:-}" = "--target" ]; then
    TARGET="${2:?--target needs a triple}"
    shift 2
fi
readonly TARGET

if [ "$#" -lt 1 ]; then
    echo "usage: $0 [--target TRIPLE] OUT_DIR [BINARY...]" >&2
    exit 2
fi
mkdir -p "$1"
readonly OUT="$(cd "$1" && pwd)"
shift
wanted=("$@")
[ "${#wanted[@]}" -gt 0 ] || wanted=("${ALL[@]}")

step() { printf '==> %s\n' "$*" >&2; }

# The first `version = "..."` in a manifest: [workspace.package]'s in the
# workspace, [package]'s in whatsapp-rust.
manifest_version() {
    sed -n 's/^version = "\(.*\)"$/\1/p' "$1" | head -n 1
}

readonly VERSION="$(manifest_version "$REPO/Cargo.toml")"
whatsapp_rust_version="$(manifest_version "$REPO/connectors/whatsapp-rust/Cargo.toml")"
if [ "$whatsapp_rust_version" != "$VERSION" ]; then
    echo "connectors/whatsapp-rust/Cargo.toml says $whatsapp_rust_version, the workspace $VERSION (scripts/release.sh sets both)" >&2
    exit 1
fi

# Where cargo puts a release build, and what cargo and go are told to target.
cargo_target=()
release_dir=release
go_env=()
if [ -n "$TARGET" ]; then
    cargo_target=(--target "$TARGET")
    release_dir="$TARGET/release"
    case $TARGET in
        x86_64-*-linux-*) go_env=(GOOS=linux GOARCH=amd64) ;;
        aarch64-*-linux-*) go_env=(GOOS=linux GOARCH=arm64) ;;
        *) echo "no Go target known for $TARGET" >&2; exit 2 ;;
    esac
    triple_var=${TARGET//-/_}
    cc_var="CC_$triple_var" cxx_var="CXX_$triple_var"
    [ -z "${!cc_var:-}" ] || go_env+=("CC=${!cc_var}")
    [ -z "${!cxx_var:-}" ] || go_env+=("CXX=${!cxx_var}")
fi

# A Go binary: its module, and whether it needs cgo.
go_module() {
    case $1 in
        wuphf-telegram) echo "connectors/telegram 0" ;;
        wuphf-whatsapp) echo "connectors/whatsapp 1" ;;
        wuphf-instagram) echo "connectors/instagram 0" ;;
        wuphf-mediaconv) echo "providers/mediaconv 1" ;;
        wuphf-misc) echo "misc 0" ;;
        *) return 1 ;;
    esac
}

build_go() {
    local binary=$1 module cgo
    read -r module cgo < <(go_module "$binary")
    step "$binary $VERSION"
    (cd "$REPO/$module" && env "${go_env[@]}" CGO_ENABLED=$cgo go build -trimpath \
        -ldflags "-s -w -X github.com/wuphf-rs/wuphf-ai/connectors/shared/version.Version=$VERSION" \
        -o "$OUT/$binary" "./cmd/$binary")
}

core=()
for binary in "${wanted[@]}"; do
    case $binary in
        wuphf-server) core+=(-p wuphf-cli) ;;
        wuphf-tui) core+=(-p wuphf-tui) ;;
        wuphf-whatsapp-rust) ;;
        *) go_module "$binary" >/dev/null || { echo "unknown binary: $binary (one of: ${ALL[*]})" >&2; exit 2; } ;;
    esac
done

if [ "${#core[@]}" -gt 0 ]; then
    step "core server and TUI $VERSION"
    cargo build --release --locked "${cargo_target[@]}" --manifest-path "$REPO/Cargo.toml" "${core[@]}"
fi
for binary in "${wanted[@]}"; do
    case $binary in
        wuphf-server) install -m 0755 "$REPO/target/$release_dir/wuphf-cli" "$OUT/wuphf-server" ;;
        wuphf-tui) install -m 0755 "$REPO/target/$release_dir/wuphf-tui" "$OUT/wuphf-tui" ;;
        wuphf-whatsapp-rust)
            # whatsapp-rust enables the nightly-only portable_simd feature (its
            # default "simd" feature). RUSTC_BOOTSTRAP lets a stable compiler
            # accept the feature gate, so no nightly toolchain is needed.
            step "wuphf-whatsapp-rust $VERSION (nightly features via RUSTC_BOOTSTRAP)"
            RUSTC_BOOTSTRAP=1 cargo build --release --locked "${cargo_target[@]}" \
                --manifest-path "$REPO/connectors/whatsapp-rust/Cargo.toml"
            install -m 0755 "$REPO/connectors/whatsapp-rust/target/$release_dir/wuphf-connector-whatsapp" \
                "$OUT/wuphf-whatsapp-rust"
            ;;
        *) build_go "$binary" ;;
    esac
done

step "built into $OUT"
ls -1 "$OUT" >&2
