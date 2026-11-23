#!/usr/bin/env bash
# Build release binaries of every component (or of the BINARY arguments) and
# install them into scripts/dev-bin/.bin/, where the dev-bin wrappers
# (wuphf-server, wuphf-telegram, …) run them in place of `cargo run` /
# `go run`. Re-run after pulling or changing code: the wrappers run whatever is
# installed, however old. Remove scripts/dev-bin/.bin/ to go back to building
# on every start.
#
#   scripts/install-dev-bins.sh [BINARY...]
#
# Run inside the nix dev shell (cargo, go, and a C/C++ toolchain for the cgo
# connectors and the mediaconv provider). The build is scripts/build-release.sh's.
set -euo pipefail

readonly REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
exec "$REPO/scripts/build-release.sh" "$REPO/scripts/dev-bin/.bin" "$@"
