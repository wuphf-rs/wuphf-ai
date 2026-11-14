# Plan 049: Move to a botconf release that lifts the gotd, tonic and prost pins

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Build limits (the owner's laptop)**: prefix every cargo command with
> `nice -n 19` and set `CARGO_BUILD_JOBS=2`; Go with `GOMAXPROCS=2
> GOFLAGS=-p=2 nice -n 19`, one module at a time. whatsapp-rust
> (`connectors/whatsapp-rust`, `RUSTC_BOOTSTRAP=1`) is the heaviest build:
> once, alone.

## Status

- **Priority**: P3
- **Effort**: M (wuphf side; the botconf release is separate work)
- **Risk**: MED — tonic 0.14 changes code generation and touches every gRPC
  service in the core and both SDKs
- **Depends on**: a botconf release (newer than v0.10.0) built on gotd ≥
  v0.162 and on tonic / prost 0.14 — work in the botconf repository
  (`github.com/propheci/botconf`), not here
- **Category**: dependencies
- **Planned at**: 2026-11-14, after the "latest dependencies" commits
  (`56963bd` Go, `1562b37` Rust)

## Why this matters

The 2026-11-14 update moved every dependency to its newest version except
these, which botconf v0.10.0 (its newest release) holds back:

| Pin | Held at | Newest | Why |
|---|---|---|---|
| `github.com/gotd/td` (`connectors/telegram`) | v0.161.0 | v0.166.0 | botconf's `go/render/telegram` (`bot.go`, `keyboard.go`) uses `tg.KeyboardButtonClass`, `tg.KeyboardButtonCallback`, `tg.KeyboardButtonRequestPeer` and `[]tg.KeyboardButtonRow` in a `ReplyInlineMarkup`; gotd v0.162.0 renamed the inline-keyboard types (`KeyboardInlineButtonRow`, …). |
| `tonic` / `tonic-build` (Rust workspace) | 0.12 | 0.14 | `botconf-grpc` v0.10.0 is built on tonic 0.12, and the core serves its settings service from the same `tonic::transport::Server` (`crates/wuphf-cli/src/main.rs`); two tonic versions cannot share a server. |
| `prost` | 0.13 | 0.14 | Moves with tonic (botconf's workspace pins `prost = "0.13"`). |

All Dependabot alerts are closed without these: gotd v0.161.0 has the
pre-auth DoS fix (≥ v0.145.1). This plan is hygiene, not a security fix.

## Prerequisite (botconf repository, not this repo)

A botconf release, tagged the same for Rust and Go (e.g. `v0.11.0` and
`go/v0.11.0`), that:

1. builds `go/render/telegram` against gotd ≥ v0.162 (the renamed keyboard
   types);
2. moves its Rust workspace to tonic / prost 0.14: `tonic-build` is replaced
   by `tonic-prost-build` (build scripts) plus the `tonic-prost` runtime
   crate in 0.14.

Check before starting: `git ls-remote --tags https://github.com/propheci/botconf`
shows the new tag; its root `Cargo.toml` says `tonic = "0.14"`, `prost =
"0.14"`; its `go/go.mod` requires gotd ≥ v0.162.0.

## Steps

### Step 1: Go — botconf and gotd

In `connectors/shared`, `connectors/telegram`, `connectors/whatsapp`,
`connectors/instagram` (each requires `github.com/propheci/botconf/go`):
`go get github.com/propheci/botconf/go@<new tag>`; in `connectors/telegram`
also `go get github.com/gotd/td@latest`; then `go mod tidy` in each.
Fix any gotd API change in `connectors/telegram/internal/telegram` (v0.161
already needed `AsInputDocumentFileLocation("")`; read gotd's changelog for
v0.162–latest).

**Verify**: each module's `go vet ./...` and `go test ./...` → green
(Telegram's `note_test.go`, `chatstate_test.go` and `panel_test.go` drive
the API through a recording invoker, so renamed request types show up
there).
Commit: `build(go): botconf <tag> and the newest gotd`.

### Step 2: Rust — botconf, tonic and prost

`Cargo.toml` `[workspace.dependencies]`: the three botconf crates to the new
tag; `tonic = "0.14"`, `prost = "0.14"`; replace `tonic-build = "0.12"` with
`tonic-prost-build = "0.14"` and add `tonic-prost = "0.14"`.

- `crates/wuphf-proto/build.rs`: `tonic_build::configure()` →
  `tonic_prost_build::configure()`; `crates/wuphf-proto/Cargo.toml` takes
  `tonic-prost` (generated code calls it) and the new build dependency.
- Fix the compile errors this causes across `wuphf-runtime`, `wuphf-cli`,
  `wuphf-connector`, `wuphf-functions` and `wuphf-tui` (tonic 0.14 API
  changes: server/transport builder, interceptors, `Status`, codegen paths).
  Keep behavior identical: no protocol change, so `proto/go` is untouched.
- `cargo update`.

**Verify**: `cargo test --workspace` → the same count as before (352 at
`1562b37`, 2 ignored); clippy `-D warnings` and fmt clean.
Commit: `build(rust): botconf <tag>, tonic and prost 0.14`.

### Step 3: whatsapp-rust

It depends on `wuphf-connector` by path, so it moves to tonic 0.14 with
Step 2. In `connectors/whatsapp-rust`: `cargo update`, then one `cargo test`
and one `cargo clippy --all-targets -- -D warnings` (with `RUSTC_BOOTSTRAP=1`).
Keep its whatsapp-rust git revision and its exact `diesel` pin. Check
`git diff Cargo.lock` only gains tonic/prost 0.14 and their dependencies.

**Verify**: 44 tests (at `1562b37`), clippy clean.
Commit: `build(whatsapp-rust): tonic and prost 0.14`.

### Step 4: Gate and index

Rust workspace, whatsapp-rust and every Go module green (mediaconv's
animated-WebP tests fail on machines whose ffmpeg lacks `libwebp_anim`;
environment, not code). Remove the three pins from the memory note and the
"held back" wording in the commit history's follow-ups, mark row 049 DONE.

## STOP conditions

- No botconf release with both changes exists yet (the prerequisite).
- botconf's new Go module renders differently (changed panel output) in a
  way `connectors/telegram`'s panel tests catch: report, do not rewrite the
  tests to match.
- tonic 0.14 forces a protocol or behavior change (a field, a status code, a
  header) rather than an API change.
- whatsapp-rust's lock would gain packages other than tonic/prost 0.14 and
  their own dependencies.

## Out of scope

The botconf code changes themselves; Instagram's `quic-go` pin (v0.61.0,
held by `mautrix-meta`'s `beeper/req` fork — it lifts when mautrix-meta
moves); the whatsapp-rust library revision.
