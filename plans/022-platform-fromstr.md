# Plan 022: Move the Platform name mapping into wuphf-model

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 52c5084..HEAD -- crates/wuphf-model/src/routing.rs crates/wuphf-cli/src/main.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: tech-debt
- **Planned at**: commit `52c5084`, 2026-07-07

## Why this matters

The mapping between platform names and the `Platform` enum lives in the CLI
(`platform()` in `wuphf-cli/src/main.rs`), away from the type it belongs to.
The proto boundary keeps its own numeric-enum mapping in
`wuphf-runtime/src/grpc.rs` (correct — that's a wire concern), but the
*string* form is a property of the domain type: config files parse it, logs
and the observer surface print connector platforms. Adding a platform today
means remembering the CLI's private function. Moving the parse (and its
inverse) onto `Platform` in `wuphf-model` puts the enumeration next to the
enum, makes it testable, and gives future consumers (e.g. connector configs,
the TUI) one canonical spelling.

## Current state

- `crates/wuphf-model/src/routing.rs:11-20` — the enum:

```rust
pub enum Platform {
    WhatsApp,
    Telegram,
    Discord,
    Signal,
    Slack,
    Matrix,
    Instagram,
    Other(String),
}
```

  It derives `Clone, Debug, PartialEq, Eq, Serialize, Deserialize` (plus
  `Hash` — check the actual derive list on the line above the enum and keep
  it). No `FromStr`/`Display` impls exist in the crate.

- `crates/wuphf-cli/src/main.rs:161-173` — the mapping to move:

```rust
fn platform(name: &str) -> Result<Platform> {
    match name {
        "whatsapp" => Ok(Platform::WhatsApp),
        "telegram" => Ok(Platform::Telegram),
        "discord" => Ok(Platform::Discord),
        "signal" => Ok(Platform::Signal),
        "slack" => Ok(Platform::Slack),
        "matrix" => Ok(Platform::Matrix),
        "instagram" => Ok(Platform::Instagram),
        _ if name.starts_with("other:") => Ok(Platform::Other(name[6..].to_owned())),
        _ => bail!("unsupported platform {name}"),
    }
}
```

  Called once, in `serve()` at `main.rs:107`.

- Not in scope but related (leave them): `platform_from_proto` in
  `crates/wuphf-runtime/src/grpc.rs:469-480` (proto i32 ↔ enum — wire
  mapping); the TUI's two-letter display abbreviations in
  `crates/wuphf-tui/src/ui.rs:257-264` (presentation, and proto-side).

- Conventions: `wuphf-model` uses `thiserror` for error types (see
  `StorageError` in wuphf-storage for style); model types carry doc comments
  explaining semantics.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Model tests | `cargo test -p wuphf-model` | exit 0 |
| Full suite | `cargo test --workspace` | exit 0; 52+ tests pass |

Note: the build needs `protoc` on PATH (`brew install protobuf` on macOS).

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-model/src/routing.rs`
- `crates/wuphf-cli/src/main.rs`

**Out of scope** (do NOT touch, even though they look related):
- `crates/wuphf-runtime/src/grpc.rs` — proto enum mapping is a wire concern
  and must stay at the wire boundary.
- `crates/wuphf-tui/**` — display abbreviations are presentation.
- The `Serialize`/`Deserialize` derives on `Platform` — rules/config that
  serde the enum depend on the current representation; `FromStr` is additive,
  not a serde change.
- The accepted name strings themselves (`"whatsapp"`, `"other:*"`, …) —
  config files in the wild use them; behavior must be identical.

## Git workflow

- Branch: `advisor/022-platform-fromstr` off `v2`.
- One commit: `refactor: move platform name parsing onto the Platform model type`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Implement `FromStr` and `Display` on `Platform`

In `crates/wuphf-model/src/routing.rs`, add:

- `impl std::str::FromStr for Platform` with
  `type Err = UnknownPlatform;` — exact same accepted strings and `other:`
  prefix rule as the CLI function above. `UnknownPlatform` is a small
  `thiserror` struct: `#[error("unsupported platform {0}")] pub struct UnknownPlatform(pub String);`
- `impl std::fmt::Display for Platform` producing the same lowercase names
  (`Other(name)` → `other:{name}`), so parse/display round-trip.

**Verify**: `cargo test -p wuphf-model` → exit 0.

### Step 2: Use it from the CLI

In `crates/wuphf-cli/src/main.rs`, delete the private `platform()` function
and replace its call site with `name.parse::<Platform>()?` (anyhow converts
the error via `?` only if `UnknownPlatform: std::error::Error` — it is, via
thiserror; use `.map_err(anyhow::Error::from)` or `.context(...)` if type
inference needs help).

**Verify**: `cargo test --workspace` → exit 0;
`grep -n "fn platform" crates/wuphf-cli/src/main.rs` → no matches.

## Test plan

In `crates/wuphf-model/src/routing.rs` (inline `#[cfg(test)] mod tests` if the
file has none — match the inline-tests style used in
`crates/wuphf-runtime/src/observation.rs:113`):

1. Every named platform parses (`"whatsapp"` → `Platform::WhatsApp`, …, all
   seven).
2. `"other:mattermost"` → `Platform::Other("mattermost".into())`.
3. `"unknown"` → `Err(UnknownPlatform)`.
4. Round-trip: for each variant, `parsed == variant.to_string().parse().unwrap()`.

**Verification**: `cargo test -p wuphf-model` → all pass, including 4 new tests.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `cargo test --workspace` exits 0, including the new model tests
- [ ] `grep -n "fn platform" crates/wuphf-cli/src/main.rs` → no matches
- [ ] `grep -n "impl std::str::FromStr for Platform" crates/wuphf-model/src/routing.rs` → one match
- [ ] `git status` shows no modified files outside the in-scope list
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `Platform` already has a `FromStr`/`Display` impl somewhere (search the
  workspace first) — reconcile instead of duplicating.
- Adding `Display` changes any existing formatting behavior (search for
  `%platform` / `{platform}` tracing or format usages and check none relied on
  the `Debug` form via a `Display` bound — as of `52c5084` there are none).
- The CLI call-site replacement changes an error message asserted by a test.

## Maintenance notes

- New platforms: add the enum variant, the `FromStr` arm, the `Display` arm,
  and the proto mapping in `grpc.rs` — the compiler's exhaustiveness check on
  `Display` will point at forgotten arms.
- Reviewer should scrutinize: accepted strings byte-identical to the old CLI
  function; `other:` prefix slicing unchanged (`name[6..]`).
- Deferred: unifying the Go connectors' platform registration constants —
  they set the proto enum directly and have no string mapping to consolidate.
