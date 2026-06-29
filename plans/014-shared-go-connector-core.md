# Plan 014: Extract the triplicated Go connector session client into one shared module

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- connectors/ proto/go/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (cross-module Go refactor; mitigated by identical-behavior tests from plan 013)
- **Depends on**: plans/013-instagram-core-client-tests.md (characterization before merge)
- **Category**: tech-debt
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

Each of the three Go connectors carries its own copy of the core session
client (`internal/core/client.go`: reconnect loop with exponential backoff,
bounded send retry, permanent-error dead-lettering, frame dispatch) and an
**identical** logging package. Measured drift at `15aa85f`:
whatsapp↔instagram differ by 1 line (platform enum); telegram differs only by
(a) a `tgerr` 400 check in `isPermanent` and (b) `RetitleThread` support. The
`internal/logging/logging.go` files are byte-identical ×3. Every fix to
backoff, ack, or dead-letter semantics currently needs three synchronized
edits, and the copies have already begun to diverge. One shared module ends
that.

## Current state

- The three copies:
  - `connectors/telegram/internal/core/client.go` (270 lines) — extra:
    `isPermanent` also treats Telegram RPC code 400 as permanent
    (client.go:33-46 region, uses `github.com/gotd/td/tgerr`); `Sender` has a
    4th method `RetitleThread(...)` and the frame loop handles
    `ServerFrame_MessageRetitle`.
  - `connectors/whatsapp/internal/core/client.go` (244 lines) — the base shape.
  - `connectors/instagram/internal/core/client.go` (244 lines) — equals
    whatsapp modulo `Platform_PLATFORM_INSTAGRAM` (registration frame,
    client.go:111).
- The shared shape (from the instagram copy, representative):
  `Client{cfg config.File, logger zerolog.Logger}` / `New(cfg, logger)` /
  `Run(ctx, sender, inputs)` (client.go:74-91) / `runSession` (93-180) /
  `deliver` (186-219) / `sendWithRetry` (223-244); constants at 16-21;
  `PermanentError`/`Permanent`/`isPermanent` at 23-42. The only config fields
  used: `cfg.CoreAddress`, `cfg.ConnectorID`, `cfg.SupportsThreads`,
  `cfg.SupportsTopicCreate` (each connector has its own `internal/config`
  package — the shared client must not import any of them).
- Logging: `connectors/{telegram,whatsapp,instagram}/internal/logging/logging.go`
  — 35 lines each, byte-identical (`diff` confirms), zerolog console logger +
  `WUPHF_LOG_LEVEL` override.
- Module layout: each connector is its own Go module
  (`github.com/akshettrj/wuphf/connectors/<name>`), all `go 1.25.0`, each with
  `replace github.com/akshettrj/wuphf/proto/go => ../../proto/go`. The proto
  module (`proto/go/go.mod`) depends only on grpc+protobuf — do NOT add
  zerolog to it; create a new module instead.
- Tests that pin behavior (must pass after the move, relocated):
  telegram `client_test.go` (115 ln), whatsapp `client_test.go` (69 ln),
  instagram `client_test.go` (plan 013, 4 tests incl. permanent
  short-circuit).
- Each connector also has `internal/core/media.go` (broker client). Telegram's
  was read at `15aa85f` (Upload stream + unbounded Download append loop,
  media.go:55-80); verify the three are as similar as the clients before
  including them — if they diverge materially, leave media for a follow-up.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Shared module | `(cd connectors/shared && go build ./... && go vet ./... && go test ./...)` | exit 0 |
| Each connector | `(cd connectors/<name> && go build ./... && go vet ./... && go test ./...)` ×3 | exit 0 |
| Workspace tidy | `go mod tidy` in each touched module | clean diffs |

## Scope

**In scope** (create/modify):
- `connectors/shared/` (new module `github.com/akshettrj/wuphf/connectors/shared`):
  `go.mod`, `coreclient/client.go`, `coreclient/client_test.go`,
  `logging/logging.go`, optionally `coreclient/media.go`
- `connectors/{telegram,whatsapp,instagram}/go.mod` (+`go.sum`): require +
  `replace github.com/akshettrj/wuphf/connectors/shared => ../shared`
- Deletion of the three `internal/core/client.go` (+tests) and three
  `internal/logging/` packages, with call sites updated
  (`grep -rn '"github.com/akshettrj/wuphf/connectors/<name>/internal/core"' connectors/<name>/` to find them; likely `cmd/wuphf-<name>/` and the platform package)

**Out of scope** (do NOT touch):
- `proto/go` module (no new deps there).
- Any behavior change to backoff/retry constants or frame handling.
- Rust code, CI config (plan 001's matrix gains `connectors/shared` — note it
  in the PR description; updating ci.yml is in scope ONLY if plan 001 already
  landed).

## Git workflow

- Branch: `advisor/014-shared-go-core`
- Commit per step; Conventional Commits. Suggested final message:
  `refactor: extract shared connector session client and logging`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Create the shared module with the parameterized client

`connectors/shared/go.mod`:

```
module github.com/akshettrj/wuphf/connectors/shared

go 1.25.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go
```

(then `require` proto/go, grpc, zerolog — let `go mod tidy` fill versions).

`coreclient/client.go`: start from the **telegram** copy (it is the superset)
and parameterize the three variation points:

```go
// Options carries the per-connector facts the shared session client needs.
type Options struct {
	CoreAddress         string
	ConnectorID         string
	Platform            wuphfpb.Platform
	SupportsThreads     bool
	SupportsTopicCreate bool
	// IsPermanent, when set, classifies platform-specific errors as permanent
	// in addition to coreclient.Permanent wrapping (e.g. Telegram RPC 400).
	IsPermanent func(error) bool
}
```

- `New(opts Options, logger zerolog.Logger) Client` replaces the
  config-struct coupling (callers map their own config to `Options`).
- `isPermanent(err)` becomes: `PermanentError` check ∥ `opts.IsPermanent != nil && opts.IsPermanent(err)`.
- `Sender` keeps the 3 base methods. Retitle becomes an optional capability:

```go
// ThreadRetitler is implemented by senders on platforms with renameable
// threads. A MessageRetitle dispatch reaching a sender without it is
// dead-lettered as permanently unsupported.
type ThreadRetitler interface {
	RetitleThread(context.Context, *wuphfpb.MessageRetitleDispatch) (threadID string, err error)
}
```

  In the `ServerFrame_MessageRetitle` case: type-assert; if the sender does
  not implement it, call `deliver` with an op returning
  `Permanent(errors.New("connector does not support thread retitling"))`.
  (The core only routes retitles to mirror-capable connectors, so this path
  is a safety net, not a hot path.)

Move the relocated tests into `coreclient/client_test.go` (union of the three
suites: flaky retry ×3 cases, permanent short-circuit, plus telegram-specific
`IsPermanent` hook test: an error classified permanent by the hook stops
retrying).

`logging/logging.go`: copy verbatim from any connector (byte-identical).

**Verify**: `(cd connectors/shared && go build ./... && go vet ./... && go test ./...)` → exit 0.

### Step 2: Switch the Telegram connector (the superset consumer)

- `go.mod`: add the require + replace for `connectors/shared`.
- Replace imports of `internal/core` with
  `github.com/akshettrj/wuphf/connectors/shared/coreclient` and
  `internal/logging` with `.../shared/logging`; construct
  `coreclient.New(coreclient.Options{..., Platform: wuphfpb.Platform_PLATFORM_TELEGRAM, IsPermanent: telegramPermanent}, logger)`
  where `telegramPermanent` keeps the `tgerr` 400 check (it stays in the
  telegram module — the shared module must not import gotd).
- Delete `connectors/telegram/internal/core/client.go` + `client_test.go` and
  `internal/logging/`. Keep `internal/core/media.go` where it is for now
  unless Step 4 moves it.
- `go mod tidy`.

**Verify**: `(cd connectors/telegram && go build ./... && go vet ./... && go test ./...)` → exit 0.

### Step 3: Switch WhatsApp and Instagram

Same mechanical change; neither sets `IsPermanent` and neither's sender
implements `ThreadRetitler`. Delete their `internal/core/client.go`(+test)
and `internal/logging/`.

**Verify**: both modules `go build ./... && go vet ./... && go test ./...` →
exit 0.

### Step 4 (conditional): media client

`diff` the three `internal/core/media.go` files (normalize the platform word
as the audit did). If they are ≥95% identical, move one copy to
`coreclient/media.go` and delete the three; otherwise leave them and record
the divergence in the PR description.

**Verify**: all three connectors build and test green.

## Test plan

- The shared suite (Step 1) = union of the three existing suites + the
  `IsPermanent` hook case. No new behavior to test beyond that.
- Final gate: all four Go modules `go build ./... && go vet ./... &&
  go test ./...` exit 0, and
  `grep -rn 'internal/core/client.go' connectors/` finds nothing.

## Done criteria

- [ ] `find connectors/*/internal/core -name 'client.go'` → no results
- [ ] `find connectors/*/internal/logging -name '*.go'` → no results
- [ ] `connectors/shared/coreclient` exists with ≥6 tests passing
- [ ] All 4 Go modules: build, vet, test → exit 0
- [ ] If `.github/workflows/ci.yml` exists, its matrix includes `connectors/shared`
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The normalized diff between the three `client.go` files at execution time
  shows NEW divergence beyond (platform enum, tgerr-400, RetitleThread) —
  someone added platform-specific behavior; the parameterization needs
  rethinking.
- Any connector's `main`/wiring uses `internal/core` symbols this plan didn't
  inventory (`Client`, `New`, `Permanent`, `Sender`, plus media types if Step
  4 runs) — list them and stop.
- Go workspace/module resolution fails on the relative `replace` (e.g. the
  repo builds with `go.work` elsewhere) — report the build setup.

## Maintenance notes

- New connectors should depend on `connectors/shared` from day one; copying a
  client.go again is the regression to watch for in review.
- The shared module is the right home for a future bounded media download
  helper (plan 011 notes) if the per-platform media clients converge.
- Reviewer: confirm zero behavior diff by reading the shared client against
  any one deleted copy side-by-side; the diff should be exactly the Options
  plumbing and the retitle type-assertion.
