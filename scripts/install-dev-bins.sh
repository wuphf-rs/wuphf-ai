#!/usr/bin/env bash
# Build release binaries of every component and install them into
# scripts/dev-bin/.bin/, where the dev-bin wrappers (wuphf-server,
# wuphf-telegram, …) run them in place of `cargo run` / `go run`. Re-run after
# pulling or changing code: the wrappers run whatever is installed, however old.
# Remove scripts/dev-bin/.bin/ to go back to building on every start.
#
# Run inside the nix dev shell (cargo, go, and a C/C++ toolchain for the cgo
# connectors and the mediaconv provider).
set -euo pipefail

readonly REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
readonly BIN="$REPO/scripts/dev-bin/.bin"
mkdir -p "$BIN"

step() { printf '==> %s\n' "$*" >&2; }

step "core server and TUI"
cargo build --release --manifest-path "$REPO/Cargo.toml" -p wuphf-cli -p wuphf-tui
install -m 0755 "$REPO/target/release/wuphf-cli" "$BIN/wuphf-server"
install -m 0755 "$REPO/target/release/wuphf-tui" "$BIN/wuphf-tui"

# whatsapp-rust enables the nightly-only portable_simd feature (its default
# "simd" feature). RUSTC_BOOTSTRAP lets the dev shell's stable compiler accept
# the feature gate, so no separate nightly toolchain is needed.
step "whatsapp-rust connector (nightly features via RUSTC_BOOTSTRAP)"
RUSTC_BOOTSTRAP=1 cargo build --release --manifest-path "$REPO/connectors/whatsapp-rust/Cargo.toml"
install -m 0755 "$REPO/connectors/whatsapp-rust/target/release/wuphf-connector-whatsapp" "$BIN/wuphf-whatsapp-rust"

for module in connectors/telegram connectors/whatsapp connectors/instagram providers/mediaconv; do
    name="wuphf-$(basename "$module")"
    step "$name"
    (cd "$REPO/$module" && go build -o "$BIN/$name" "./cmd/$name")
done

step "installed into $BIN"
ls -1 "$BIN" >&2
