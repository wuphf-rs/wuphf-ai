# Plan 003: Validate routing rules at startup instead of panicking on first message

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- crates/wuphf-core/src/lib.rs crates/wuphf-router/src/lib.rs crates/wuphf-cli/src/main.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

A rule whose sides are all `mirror` (no `source` or `single` side) is accepted
at config load and then panics the whole server when the first message routes
through it: `identity_side_index` ends in `.expect("a rule has at least one
single or source side")`. Rules are hand-written TOML
(`config/example.toml`), so a typo (`kind = "mirror"` on both sides) is a
plausible operator error, and the failure mode — a crash mid-operation rather
than an error at startup — is the worst available. Nothing anywhere validates
rules: `RoutingConfig::new` stores them untouched.

## Current state

- `crates/wuphf-core/src/lib.rs:92-104` — the panic site:

```rust
/// The identity-owning side of a rule: the source side if one exists, otherwise
/// the first single side. Used to key the conversation deterministically.
fn identity_side_index(rule: &Rule) -> usize {
    rule.sides
        .iter()
        .position(|side| matches!(side.kind, SideKind::Source { .. }))
        .or_else(|| {
            rule.sides
                .iter()
                .position(|side| matches!(side.kind, SideKind::Single { .. }))
        })
        .expect("a rule has at least one single or source side")
}
```

- `crates/wuphf-router/src/lib.rs:36-39` — construction with no validation:

```rust
impl RoutingConfig {
    pub fn new(rules: Vec<Rule>, connectors: HashMap<ConnectorId, ConnectorInfo>) -> Self {
        Self { rules, connectors }
    }
```

- `crates/wuphf-cli/src/main.rs:117` — the only production constructor call:

```rust
    let config = Arc::new(RoutingConfig::new(configuration.rules, connector_infos));
```

- `SideKind` variants are `Single`, `Source`, `Mirror`
  (`crates/wuphf-model/src/routing.rs`, enum `SideKind`).
- Tests construct `RoutingConfig::new(...)` too: `crates/wuphf-runtime/src/tests.rs:91`
  and several sites in `crates/wuphf-core/src/tests.rs`. Changing `new`'s
  signature breaks them — that is why this plan adds a separate fallible
  constructor instead.
- Error-handling convention: library crates use `thiserror` enums
  (`RouterError` in `crates/wuphf-router/src/lib.rs:138-146`); the CLI uses
  `anyhow` with `.context(...)` (`crates/wuphf-cli/src/main.rs:86-91`). Match it.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Format | `cargo fmt --check` | exit 0 |
| Tests | `cargo test --workspace` | exit 0, all pass (25 at baseline + new) |
| Lint | `cargo clippy --workspace --all-targets` | exit 0 |
| Targeted tests | `cargo test -p wuphf-router` | new validation tests pass |

## Scope

**In scope** (the only files you should modify):
- `crates/wuphf-router/src/lib.rs` — add `RoutingConfig::validated` + error type
- `crates/wuphf-cli/src/main.rs` — call the validating constructor
- New test module or `#[cfg(test)]` block in `crates/wuphf-router/src/lib.rs`

**Out of scope** (do NOT touch):
- `crates/wuphf-core/src/lib.rs` — keep `identity_side_index` and its `expect`
  as-is; after startup validation the invariant genuinely holds, and plan 007
  adds router tests that lean on the current shape.
- `RoutingConfig::new` signature — existing tests construct valid configs
  through it; leave it working.
- Rule semantics (what counts as a valid combination beyond the one invariant).

## Git workflow

- Branch: `advisor/003-validate-rules`
- Commit style: Conventional Commits; suggested `fix: reject rules without a source or single side at startup`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add a validation error and fallible constructor to `wuphf-router`

In `crates/wuphf-router/src/lib.rs`, add:

```rust
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("rule {0} has no source or single side to own conversation identity")]
    RuleWithoutIdentitySide(RuleId),
    #[error("rule {0} has fewer than two sides")]
    RuleTooFewSides(RuleId),
}
```

and on `impl RoutingConfig`:

```rust
/// Validate rules and construct the config. Every rule must have at least
/// two sides and at least one `source` or `single` side (the side that owns
/// conversation identity — see `identity_side_index` in wuphf-core).
pub fn validated(
    rules: Vec<Rule>,
    connectors: HashMap<ConnectorId, ConnectorInfo>,
) -> Result<Self, ConfigError> {
    for rule in &rules {
        if rule.sides.len() < 2 {
            return Err(ConfigError::RuleTooFewSides(rule.id.clone()));
        }
        let has_identity_side = rule.sides.iter().any(|side| {
            matches!(side.kind, SideKind::Source { .. } | SideKind::Single { .. })
        });
        if !has_identity_side {
            return Err(ConfigError::RuleWithoutIdentitySide(rule.id.clone()));
        }
    }
    Ok(Self::new(rules, connectors))
}
```

`RuleId` needs `Clone` — check `crates/wuphf-model/src/ids.rs`; if it is not
`Clone`, use `rule.id.0.clone()` into a `String`-carrying error variant
instead.

**Verify**: `cargo build -p wuphf-router` → exit 0.

### Step 2: Use it in the CLI

In `crates/wuphf-cli/src/main.rs:117` replace:

```rust
let config = Arc::new(RoutingConfig::new(configuration.rules, connector_infos));
```

with:

```rust
let config = Arc::new(
    RoutingConfig::validated(configuration.rules, connector_infos)
        .context("invalid routing rules")?,
);
```

**Verify**: `cargo build -p wuphf-cli` → exit 0.

### Step 3: Add tests

In `crates/wuphf-router/src/lib.rs`, a `#[cfg(test)] mod config_tests` with:

1. valid rule (one `source` + one `mirror` side) → `validated` returns `Ok`.
2. rule with two `Mirror` sides → `Err(ConfigError::RuleWithoutIdentitySide)`.
3. rule with one side → `Err(ConfigError::RuleTooFewSides)`.

Build `Rule`/`RuleSide`/`SideKind` literals the way
`crates/wuphf-runtime/src/tests.rs:24-44` does (see its `rules()` helper).

**Verify**: `cargo test -p wuphf-router` → 3 new tests pass.

## Test plan

The three unit tests in Step 3. Pattern: `rules()` helper in
`crates/wuphf-runtime/src/tests.rs:24-44`. Full gate:
`cargo test --workspace` → exit 0, no existing test broken (none construct
invalid rules).

## Done criteria

- [ ] `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace` all exit 0
- [ ] `grep -n 'RoutingConfig::new' crates/wuphf-cli/src/main.rs` → no matches (CLI uses `validated`)
- [ ] Running the server with a TOML rule whose sides are both `mirror` fails at startup with a message naming the rule id (manual check: temporarily add such a rule to a scratch copy of `config/example.toml`, run `cargo run -p wuphf-cli -- serve --config <scratch>`, expect a clean error, not a panic; delete the scratch file)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `identity_side_index` in `crates/wuphf-core/src/lib.rs` no longer ends in
  `.expect(...)` (someone already fixed this differently).
- Adding `ConfigError` creates a name collision in `wuphf-router`'s public API.
- More call sites than `crates/wuphf-cli/src/main.rs:117` construct
  `RoutingConfig` from *user-supplied* (non-test) data.

## Maintenance notes

- Future rule invariants (e.g. "a mirror side requires
  `supports_threads`/`supports_topic_create` capability" — currently only
  documented in `config/example.toml` comments) belong in `validated` too.
  That check needs the `connectors` map, which `validated` already receives.
- Plan 017 (asymmetric edit/delete capability flags), if executed, adds
  capability fields — its validation belongs here.
