# Plan 029: Fold the wuphf-router crate into wuphf-core

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 5c38943..HEAD -- crates/wuphf-router crates/wuphf-core/src/lib.rs crates/wuphf-core/Cargo.toml Cargo.toml`
> Renames from plans 026–028 inside these files are expected. What must still
> hold: `crates/wuphf-router/src/` contains exactly `lib.rs` and `routing.rs`;
> the only crate depending on `wuphf-router` is `wuphf-core`
> (`grep -rn "wuphf-router" crates/*/Cargo.toml` → only the router's own
> manifest and `crates/wuphf-core/Cargo.toml`). Otherwise STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW — a file move; no logic changes
- **Depends on**: plans/028-model-dead-and-duplicate-types.md (soft: 028 edits
  `crates/wuphf-router/src/lib.rs`; landing it first avoids a conflict)
- **Category**: tech-debt
- **Planned at**: commit `5c38943`, 2026-10-21

## Why this matters

`wuphf-router` has exactly one user, `wuphf-core`, and holds more than routing:
`RoutingConfig` also carries the connector list and the owner-notice targets.
A separate crate for one caller adds a manifest, a dependency edge and a crate
boundary a reader must cross to follow one ingest call, without enforcing
anything the module system cannot. Moving the code into two modules of
`wuphf-core` keeps the separation of files and removes the crate.

## Current state

`crates/wuphf-router/Cargo.toml` depends on `thiserror`, `wuphf-model` and
`wuphf-storage`, all of which `wuphf-core` already depends on.

`crates/wuphf-router/src/lib.rs` contains, in order:
- imports (`std::collections::HashMap`, `std::sync::Arc`, `thiserror::Error`,
  model types, `wuphf_storage::{Storage, StorageError}`) and `mod routing;`
- `pub struct RoutingConfig { rules, connectors, notice_targets }` and its
  `impl` (`new`, `with_notice_targets`, `notice_targets_excluding`, `rule`,
  `connector`, `match_source`)
- `pub struct MatchedRule<'a> { pub rule: &'a Rule, pub origin: RuleEnd }`
- `pub struct Router<S> { storage: Arc<S>, config: Arc<RoutingConfig> }` and
  `impl<S: Storage> Router<S> { pub fn new(…) }`
- `fn is_mirror_side(rule: &Rule, connector: &ConnectorId, container: &str) -> bool`
- `pub enum RouterError { Storage(#[from] StorageError), OriginOutsideReplicaSet(EndpointId), MessageNotFound(MessageId) }`

`crates/wuphf-router/src/routing.rs` contains a module doc comment, then
`use crate::{Router, RouterError, is_mirror_side};`, model imports, and one
`impl<S> Router<S> where S: Storage { … }` block with `route`, `route_edit`,
`route_delete`, `route_reaction`, `route_receipt`, `fan_out_to_copies`,
`route_retitle` and `resolve_target`.

`crates/wuphf-core/src/lib.rs` uses it like this (lines 8, 11, 21, 52, 142 at
planning time):

```rust
use wuphf_router::{Router, RouterError};
pub use wuphf_router::RoutingConfig;
// …
    router: Router<S>,
// …
            router: Router::new(Arc::clone(&storage), Arc::clone(&config)),
// …
    #[error(transparent)]
    Router(#[from] RouterError),
```

`wuphf-runtime`, `wuphf-cli` and the tests import `RoutingConfig` from
`wuphf_core`, never from `wuphf_router`. That import path must keep working.

References to the crate outside code:
- root `Cargo.toml`: workspace member `"crates/wuphf-router"` and workspace
  dependency `wuphf-router = { path = "crates/wuphf-router" }`
- `README.md` "Layout" block: `wuphf-router/    Rule evaluation and target resolution`
- `docs/architecture.md` "Components" table: a `wuphf-router` row
- `docs/index.html`: a `<tr><td class="k">wuphf-router</td>…</tr>` row in the crate table

Core's own modules are one file per topic (`ingest.rs`, `resolve.rs`,
`acknowledge.rs`, `send.rs`, `notify.rs`), each starting with a `//!` doc
comment. Match that.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Build | `cargo build --workspace --all-targets` | exit 0 |
| Core tests | `cargo test -p wuphf-core` | exit 0, same count as before |
| All tests | `cargo test --workspace` | exit 0, same total passed count as before |
| Lint | `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |
| Dependency check | `cargo tree -p wuphf-core --depth 1` | no `wuphf-router` line |

## Scope

**In scope**:
- `crates/wuphf-router/**` (moved, then deleted)
- `crates/wuphf-core/src/config.rs` (create), `crates/wuphf-core/src/router.rs` (create)
- `crates/wuphf-core/src/lib.rs`, `crates/wuphf-core/Cargo.toml`
- root `Cargo.toml`, `Cargo.lock` (updated by cargo)
- `README.md`, `docs/architecture.md`, `docs/index.html` (one crate-table row each)

**Out of scope** (do NOT touch):
- The bodies of any moved function: copy them verbatim. No renames, no
  refactors, no merging `RouterError` into `CoreError`.
- Other files in `crates/wuphf-core/src/` beyond import lines (none should be
  needed: they reach the router through `self.router`).
- Older plans in `plans/` that mention `crates/wuphf-router/…` paths; the
  index notes the move.

## Git workflow

- Branch: `advisor/029-fold-router-into-core`.
- Use `git mv` for the moves so history follows the files. One commit:
  `refactor: fold wuphf-router into wuphf-core`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 0: Record the baseline

`cargo test --workspace 2>&1 | grep "^test result"`. Sum the passed counts.
The router crate has no tests, so the total must be unchanged at the end.

### Step 1: Move the files

```sh
git mv crates/wuphf-router/src/routing.rs crates/wuphf-core/src/router.rs
git mv crates/wuphf-router/src/lib.rs crates/wuphf-core/src/config.rs
```

Then split the content so each file holds one topic:
- `config.rs` keeps: `RoutingConfig` + impl and `MatchedRule`. Replace its
  first lines with a `//!` doc comment ("The rule set, connector list and
  owner-notice targets the core routes by.") and only the imports these items
  need. Remove `mod routing;`.
- Move from `config.rs` into `router.rs`: the `Router` struct and its `new`,
  `is_mirror_side`, and `RouterError`. Keep `router.rs`'s existing `//!` doc
  comment at the top.
- In `router.rs`, change `use crate::{Router, RouterError, is_mirror_side};`
  to `use crate::config::RoutingConfig;` (the three items are now in the same
  file) and add the imports the moved items need (`std::sync::Arc`,
  `thiserror::Error`, `wuphf_storage::{Storage, StorageError}`, model types).
- Make `Router` `pub(crate)`. Keep `RouterError` and `MatchedRule` `pub`
  (`CoreError::Router` exposes `RouterError` publicly).

### Step 2: Wire the modules into core

In `crates/wuphf-core/src/lib.rs`, replace the two `wuphf_router` lines with:

```rust
mod config;
mod router;

pub use config::{MatchedRule, RoutingConfig};
pub use router::RouterError;
use router::Router;
```

Keep the existing `mod acknowledge; mod ingest; …` lines.

### Step 3: Remove the crate from the workspace

- `crates/wuphf-core/Cargo.toml`: delete `wuphf-router.workspace = true`.
- Root `Cargo.toml`: delete `"crates/wuphf-router",` from `members` and the
  `wuphf-router = { path = "crates/wuphf-router" }` line.
- `git rm crates/wuphf-router/Cargo.toml` and remove the now-empty directory.

**Verify**: `cargo build --workspace --all-targets` → exit 0;
`cargo tree -p wuphf-core --depth 1 | grep wuphf-router` → no output;
`test -d crates/wuphf-router` → exit 1 (directory gone).

### Step 4: Update the docs

- `README.md` Layout block: delete the `wuphf-router/` line; change the
  `wuphf-core/` line to `wuphf-core/      Identity, rule matching, fan-out, and event ingestion`.
- `docs/architecture.md` Components table: delete the `wuphf-router` row;
  change the `wuphf-core` row to: "Owns logical identity, dedup, echo
  suppression, reply resolution and identity migration; evaluates rules and
  computes dispatch targets (`config.rs`, `router.rs`)."
- `docs/index.html`: delete the `wuphf-router` `<tr>`; append to the
  `wuphf-core` cell: " Evaluates rules and computes dispatch targets."

**Verify**: `grep -rn "wuphf-router\|wuphf_router" README.md docs crates Cargo.toml` → no matches.

### Step 5: Full gate

**Verify**: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0, total passed equals Step 0.

## Test plan

No new tests: the code moves verbatim. The 40+ core tests exercise every
router method through `Core::ingest` and `Core::acknowledge_dispatch`, and must
pass unchanged.

## Done criteria

- [ ] `test -d crates/wuphf-router` exits 1
- [ ] `grep -rn "wuphf_router\|wuphf-router" crates Cargo.toml README.md docs` → no matches
- [ ] `cargo test --workspace` exits 0 with the Step 0 total
- [ ] `cargo clippy --workspace --all-targets` exits 0 with no warnings
- [ ] `git diff --stat` shows only the in-scope paths
- [ ] `plans/README.md` status row updated

## STOP conditions

- A crate other than `wuphf-core` depends on `wuphf-router`.
- Making it compile seems to require changing a moved function's body (a
  visibility keyword or an import is fine; logic is not).
- clippy reports a new warning caused by the move (for example
  `dead_code` on a router method that was `pub` and is now crate-private).
  Report the method name rather than deleting it.
- The test total changes.

## Maintenance notes

- Plans 003, 007, 017 and 021 cite `crates/wuphf-router/src/lib.rs` and
  `routing.rs`. After this plan those items live in
  `crates/wuphf-core/src/config.rs` (`RoutingConfig`, `MatchedRule`) and
  `crates/wuphf-core/src/router.rs` (`Router`, `RouterError`,
  `is_mirror_side`). Plan 007's "router crate unit tests" become core unit
  tests.
- Plan 030 rewrites `router.rs` heavily; doing this move first keeps that
  diff in one crate.
