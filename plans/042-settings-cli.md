# Plan 042: A `wuphf settings` command

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- crates/wuphf-cli Cargo.toml`
> Plans 035–040 are expected to have changed `crates/wuphf-cli/src/main.rs`.
> What must still hold: `Command` is a clap `Subcommand` enum with `Serve`
> and `Functions`; the settings store is built by one function from the
> config path and the database (035/036), which `serve()` calls; the
> settings service is mounted in `serve()` (039). Otherwise STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW — a new command; the store and its validation already exist
- **Depends on**: 035, 036 (store + storage), 039 (settings service, for
  `reload`); 040 optional (connector sections show up once it lands); the
  botconf tag with the `cli` feature
- **Category**: DX
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

The owner needs to view and change the runtime settings from a terminal,
including while the core is down (for example to fix a rule that stops it
routing). botconf ships a clap subcommand behind its `cli` feature
(`botconf::cli::SettingsAction`, `botconf::cli::render`) that the assistant
bot already uses the same way; wuphf only has to build its store and call
`botconf::command::execute`.

## Design decisions

| # | Decision |
|---|---|
| 1 | (Settled) The CLI edits the **database directly** through the same `SettingsStore<WuphfSchema>` the core uses, so every change is validated exactly as at runtime. It works while the core is down. |

### Decisions made by this plan (confirmed by the owner 2026-10-25)

| # | Decision | Why |
|---|---|---|
| F1 | Add `wuphf settings reload --address <url>` that calls the running core's `Settings.Reload`, authenticated with the observer token from `WUPHF_OBSERVER_TOKEN` (039 F1). The other actions stay local-database only; there is no remote `set`. | A local change needs a way to reach a running core without restarting it; a remote `set` would duplicate the TUI and panels and needs the same token anyway. |
| F2 | After a successful local change, print to stderr: `note: a running core applies this after "wuphf settings reload" or a restart`. | Same wording as the assistant bot's CLI (`app.rs:60`). |

## Current state

- `crates/wuphf-cli/src/main.rs:25-53` — `Arguments { command: Command }`;
  `Command::Serve { config: PathBuf }` (default `config/example.toml`) and
  `Command::Functions { command: FunctionsCommand::List { address } }`.
- `crates/wuphf-cli/src/main.rs:206-249` — `list_functions(address)` is the
  model for a client command: `FunctionTransportClient::connect`, bearer token
  from the environment (never a flag, so it stays out of `ps`).
- botconf `cli` feature (assistant bot's `crates/botconf/src/cli.rs`):

  ```rust
  #[derive(Subcommand)]
  pub enum SettingsAction { List, Get { key }, Set(KeyValue), Unset { key }, Add(KeyValue), Remove(KeyValue) }
  impl From<SettingsAction> for SettingsCommand { … }
  pub fn render<S: Schema>(outcome: &Outcome<S>) -> String;
  ```
  The assistant bot calls it as
  `command::execute(&settings, action.into(), None).await?` then
  `println!("{}", botconf::cli::render(&outcome))` (`src/app.rs:50-62`).
- botconf-grpc (plan 005) generates a `SettingsClient` with a `reload` RPC
  (name may differ; STOP if there is no reload RPC).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| CLI tests | `cargo test -p wuphf-cli` | exit 0 |
| All tests | `cargo test --workspace` | exit 0; baseline + new |
| Lint | `cargo clippy --workspace --all-targets && cargo fmt --check` | exit 0 |
| Smoke | see Step 3 | as listed |

## Scope

**In scope**: `crates/wuphf-cli/src/main.rs` (or a new
`crates/wuphf-cli/src/settings.rs`), `crates/wuphf-cli/Cargo.toml` (enable
`botconf`'s `cli` feature), `README.md`, `docs/architecture.md`.

**Out of scope**: the store, schema, storage and service (035–040); the TUI.

## Git workflow

- Commit on the current branch. Commits:
  1. `feat(cli): add wuphf settings`
  2. `docs: describe wuphf settings`
- Do NOT push.

## Steps

### Step 0: Baseline

Record `cargo test --workspace` total.

### Step 1: The subcommand

```rust
/// View or change the runtime settings. Edits the database directly, so it
/// works while the core is down.
Settings {
    #[arg(long, default_value = "config/example.toml")]
    config: PathBuf,
    #[command(subcommand)]
    action: SettingsCommand,
},

#[derive(Subcommand)]
enum SettingsCommand {
    #[command(flatten)]
    Local(botconf::cli::SettingsAction),
    /// Ask a running core to re-read its file and the database. Reads the
    /// token from WUPHF_OBSERVER_TOKEN.
    Reload {
        #[arg(long, default_value = "http://127.0.0.1:50051")]
        address: String,
    },
}
```

`Local(action)`: build the store with the same function `serve()` uses
(config path → figment → `SqliteStorage` → `SettingsStore<WuphfSchema>`,
running migrations as `serve` does), call `botconf::command::execute(&store,
action.into(), None)`, print `render(&outcome)` to stdout, and F2's note to
stderr when `outcome.change().is_some()`. The store must be built **without**
starting any server or hub.

`Reload { address }`: connect the botconf-grpc client, attach
`authorization: Bearer $WUPHF_OBSERVER_TOKEN` when set, call reload, print
`reloaded: <n> overrides, <m> ignored`.

**Verify**: `cargo run -p wuphf-cli -- settings --help` → lists `list`, `get`,
`set`, `unset`, `add`, `remove`, `reload`.

### Step 2: Tests

In `wuphf-cli` tests (temp directory, temp SQLite file, the example config
with `database_url` pointed at it):

1. `settings_set_then_list_shows_stored_value` — `set` a runtime key, then
   `list` output contains the key with source `stored`.
2. `settings_set_rejects_invalid_value_and_stores_nothing`.
3. `settings_unset_falls_back_to_file`.
4. `settings_reload_calls_running_core` — start the settings service on
   `127.0.0.1:0` (039's harness) with an observer token, run the reload path
   with the token, and assert the store picked up a change written directly
   to the database beforehand.
5. `settings_reload_without_token_is_refused` — `UNAUTHENTICATED` surfaces
   as a non-zero exit with the status message.

Factor the command bodies into functions taking the store/address so tests
call them without spawning the binary.

**Verify**: `cargo test -p wuphf-cli settings` → 5 pass.

Commit 1.

### Step 3: Smoke

On a **copy** of the local database (`cp wuphf.sqlite /tmp/…/wuphf-copy.sqlite`
and a config pointing at the copy; never the original):

- `cargo run -p wuphf-cli -- settings --config <copy config> list` → exit 0,
  prints the runtime settings.
- `… settings --config <copy config> get rules` → exit 0.

### Step 4: Document

`README.md` CLI section and `docs/architecture.md` "Settings" paragraph: the
command, that it edits the database directly, and `reload`.

Commit 2.

### Step 5: Full gate

`cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
→ exit 0.

## Done criteria

- [ ] The five tests exist and pass; clippy and fmt clean
- [ ] The smoke run touched only a copy of `wuphf.sqlite`
- [ ] `plans/README.md` row updated; F1–F2 recorded

## STOP conditions

- Building the store needs the server, the hub or the runtime to start.
- botconf's tagged `cli` feature has no `SettingsAction`/`render`, or
  botconf-grpc has no reload RPC.
- The local store and a running core disagree about validation (they must
  share one `WuphfSchema`).

## Maintenance notes

- Connector-declared sections (040) appear in `list` because the declarations
  are stored in the database; editing them needs no connector running.
- A remote `set` can be added later by reusing the botconf-grpc client.
