#!/usr/bin/env bash
# Regenerates the Go bindings (proto/go) from crates/wuphf-proto/*.proto.
# Needs protoc and its Go plugins: run it inside `nix develop`.
set -euo pipefail
cd "$(dirname "$0")/../crates/wuphf-proto"
module=github.com/wuphf-rs/wuphf-ai/proto/go
protoc \
  --go_out=../../proto/go --go_opt=module="$module" \
  --go-grpc_out=../../proto/go --go-grpc_opt=module="$module" \
  common.proto media.proto client.proto server.proto connector.proto functions.proto observer.proto plugin.proto
