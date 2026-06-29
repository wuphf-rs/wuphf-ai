# Plan 001: Add a CI pipeline that runs every documented verification command

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- README.md .github/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: dx
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

The repo has no CI at all (no `.github/workflows/` directory). Verification is
manual: README.md lines 61-71 list six commands a contributor must remember to
run. The router and core crates are high-churn (9+ commits each in the last 30)
and a regression in routing or dispatch state survives until someone runs
`cargo test` by hand. CI is purely additive — it changes no source code and
unblocks every riskier plan in this series by making regressions visible.

## Current state

- No `.github/` directory exists at the repo root.
- `README.md:61-71` documents the verification commands:

```sh
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets
(cd proto/go && go test ./...)
(cd connectors/telegram && go test ./...)
(cd connectors/whatsapp && go test ./...)
nix flake check --all-systems
```

- The repo is a Rust 2024 workspace (root `Cargo.toml`, 7 crates under
  `crates/`) plus three Go modules (`connectors/telegram`, `connectors/whatsapp`,
  `connectors/instagram`) and a shared Go module `proto/go`. Note the README
  omits `connectors/instagram` — CI must include it (plan 002 fixes the README).
- `crates/wuphf-proto/build.rs` compiles protobufs with `tonic-build`, which
  requires `protoc` on PATH at build time.
- Go version per `connectors/*/go.mod`: `go 1.25.0`. Rust edition 2024 (needs a
  recent stable toolchain, 1.85+).
- All test suites pass at the planned-at commit: 25 Rust tests, all Go package
  tests.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Rust fmt | `cargo fmt --check` | exit 0, no diff |
| Rust tests | `cargo test --workspace` | exit 0, 25+ tests pass |
| Rust lint | `cargo clippy --workspace --all-targets` | exit 0 |
| Go tests | `(cd connectors/<name> && go test ./...)` for telegram, whatsapp, instagram; `(cd proto/go && go test ./...)` | exit 0 |
| Workflow syntax | `gh workflow list` after push, or a YAML linter locally | workflow parses |

## Scope

**In scope** (the only files you should create/modify):
- `.github/workflows/ci.yml` (create)

**Out of scope** (do NOT touch):
- Any source file in `crates/` or `connectors/`.
- `flake.nix` — do not add a `nix flake check` job; GitHub runners lack the Nix
  setup and adding it is a separate decision (see Maintenance notes).
- README.md (plan 002 owns doc updates).

## Git workflow

- Branch: `advisor/001-ci-pipeline`
- Commit style: Conventional Commits, matching `git log` (e.g.
  `feat: add structured logging to the core server and all connectors`).
  Suggested: `feat: add CI workflow running Rust and Go verification`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Create the workflow file

Create `.github/workflows/ci.yml` with two jobs:

```yaml
name: CI

on:
  push:
    branches: [main, v2]
  pull_request:

jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: arduino/setup-protoc@v3
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace

  go:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        module: [proto/go, connectors/telegram, connectors/whatsapp, connectors/instagram]
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-go@v5
        with:
          go-version: '1.25'
          cache-dependency-path: ${{ matrix.module }}/go.sum
      - run: go build ./...
        working-directory: ${{ matrix.module }}
      - run: go vet ./...
        working-directory: ${{ matrix.module }}
      - run: go test ./...
        working-directory: ${{ matrix.module }}
```

`protoc` setup must come before any cargo invocation: `wuphf-proto/build.rs`
fails without it.

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` locally → exit 0. If `-D warnings`
fails locally on pre-existing warnings, drop `-- -D warnings` from both the
local run and the workflow (record this in the PR description) — do not fix
the warnings in this plan.

### Step 2: Verify the Go matrix commands locally

Run for each module:

```sh
(cd proto/go && go build ./... && go vet ./... && go test ./...)
(cd connectors/telegram && go build ./... && go vet ./... && go test ./...)
(cd connectors/whatsapp && go build ./... && go vet ./... && go test ./...)
(cd connectors/instagram && go build ./... && go vet ./... && go test ./...)
```

**Verify**: every command exits 0. If `go vet` reports pre-existing issues,
remove the vet step from the workflow and note it — do not fix vet findings
in this plan.

## Test plan

No new tests — this plan adds the harness that runs existing ones. The
verification is that the workflow file's commands all pass locally at the
planned-at commit.

## Done criteria

- [ ] `.github/workflows/ci.yml` exists and contains both jobs
- [ ] Every command in the workflow has been run locally and exits 0
- [ ] `git status` shows no modified files outside `.github/` and `plans/`
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `cargo test --workspace` fails at the planned-at commit (baseline broken —
  CI would be born red).
- More than 5 clippy warnings appear with `-D warnings` (signals a cleanup
  plan is needed first; ship the workflow without `-D warnings` and report).
- You find an existing CI config in a non-GitHub location (e.g. `.gitlab-ci.yml`,
  `.forgejo/`) — the repo may not be hosted on GitHub; report which host.

## Maintenance notes

- `nix flake check --all-systems` was deliberately left out; if the project
  wants it in CI, add a separate job using `DeterminateSystems/nix-installer-action`.
- When a new Go connector is added, add its path to the `matrix.module` list.
- Plans 004-014 rely on this workflow to catch regressions; land this first.
