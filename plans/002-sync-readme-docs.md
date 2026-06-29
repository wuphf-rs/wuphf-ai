# Plan 002: Sync README with the code — Instagram connector and executed retitling

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- README.md docs/architecture.md`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW (docs only)
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

README.md is actively wrong in two places: it omits the Instagram connector
(which has existed since commit `601c398` and was expanded with media, edits,
deletes, and titles in `50aedab`), and it claims mirror-thread retitling is
"reserved but not yet executed" although retitling landed in `f14df76` and was
fixed in `e0fde95`. `docs/architecture.md` is already correct on both points —
the README contradicts it. Stale docs are worse than missing docs: a new user
reading only the README concludes Instagram is unsupported.

## Current state

- `README.md:26-27`:

```text
- Go Telegram (`gotd/td`, forum topics) and WhatsApp (`whatsmeow`, QR pairing)
  connectors for text and media.
```

- `README.md:29-30`:

```text
Reactions and mirror-thread retitling are modelled or reserved but not yet
executed.
```

- `README.md:43` (Layout section):

```text
connectors/        Go Telegram (gotd/td) and WhatsApp (whatsmeow) connectors
```

- `README.md:49-59` (Run section) shows run commands for telegram and whatsapp
  connectors only; `connectors/instagram/cmd/wuphf-instagram` exists.
- `README.md:61-71` (Verify section) lists Go tests for telegram and whatsapp
  but not instagram.
- The correct connector description, in `docs/architecture.md:63`:

```text
| `connectors/` | Go connectors: Telegram (`gotd/td`, forum topics), WhatsApp (`whatsmeow`, QR pairing), and Instagram (`mautrix/meta`, cookie auth). |
```

- Retitling is executed: `crates/wuphf-router/src/routing.rs:216` implements
  `route_retitle`, and `docs/architecture.md:102-103` documents it as a live
  invariant. Reactions remain unexecuted (`crates/wuphf-core/src/lib.rs:87`
  returns `CoreError::UnsupportedEvent`) — keep the reactions claim.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Confirm Instagram connector exists | `ls connectors/instagram/cmd` | `wuphf-instagram` |
| Confirm retitle is implemented | `grep -n route_retitle crates/wuphf-router/src/routing.rs` | a `pub async fn` match |
| Confirm instagram tests run | `(cd connectors/instagram && go test ./...)` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `README.md`

**Out of scope** (do NOT touch):
- `docs/architecture.md`, `docs/connector-protocol.md` — already correct.
- `docs/index.html` — generated human-friendly tour; syncing it is a separate
  effort, do not hand-edit.
- Any source file.

## Git workflow

- Branch: `advisor/002-sync-readme`
- Commit style: Conventional Commits; suggested `docs: sync README with Instagram connector and retitling`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Fix the connector list (line 26-27)

Replace with:

```text
- Go Telegram (`gotd/td`, forum topics), WhatsApp (`whatsmeow`, QR pairing),
  and Instagram (`mautrix/meta`, cookie auth) connectors for text and media.
```

**Verify**: `grep -n 'mautrix/meta' README.md` → one match in the What works section.

### Step 2: Fix the stale retitling claim (line 29-30)

Replace the sentence with:

```text
Reactions are modelled but not yet executed.
```

(Retitling is live; move mention of it into the feature list if desired — e.g.
extend the routing bullet with "mirror threads are retitled when the source
conversation renames".)

**Verify**: `grep -n 'not yet' README.md` → no line mentioning retitling.

### Step 3: Fix Layout, Run, and Verify sections

- Line 43: `connectors/        Go Telegram (gotd/td), WhatsApp (whatsmeow), and Instagram (mautrix/meta) connectors`
- Run section: add `(cd connectors/instagram && go run ./cmd/wuphf-instagram --config config.example.toml)` with a one-line note that Instagram authenticates with a cookies file (see `connectors/instagram/config.example.toml`).
- Verify section: add `(cd connectors/instagram && go test ./...)`.

**Verify**: `grep -c instagram README.md` → at least 3 matches.

## Test plan

Docs-only; the verification greps above plus a full read-through of the diff.

## Done criteria

- [ ] All three greps above match as stated
- [ ] `git diff --stat` touches only `README.md` (plus `plans/README.md`)
- [ ] No remaining README claim contradicts `docs/architecture.md`
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `route_retitle` no longer exists in `crates/wuphf-router/src/routing.rs`
  (feature moved/renamed — README claim may need different wording).
- The Instagram connector directory is gone or renamed.

## Maintenance notes

- README drifts every time a connector or feature lands; consider a PR
  checklist item ("README updated?") once CI (plan 001) exists.
- `docs/index.html` likely repeats the connector list; flag it for regeneration
  but do not hand-edit.
