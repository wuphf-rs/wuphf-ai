# Plan 013: Give the Instagram core client the same tests Telegram and WhatsApp have

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- connectors/instagram/internal/core/ connectors/whatsapp/internal/core/client_test.go`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW (test-only)
- **Depends on**: none (land before plans/014, which deduplicates the three clients)
- **Category**: tests
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

All three Go connectors carry a near-identical `internal/core/client.go`
(session loop, bounded retry, permanent-error dead-lettering). Telegram and
WhatsApp test theirs (`client_test.go`, 115 and 69 lines); Instagram —
the newest connector — has none. The retry/dead-letter logic is what stands
between a flaky platform call and either lost work or infinite redelivery.
Plan 014 will merge the three copies into one shared package; Instagram needs
characterization coverage first so the merge can prove it changed nothing.

## Current state

- `connectors/instagram/internal/core/client.go` (244 lines) — differs from
  `connectors/whatsapp/internal/core/client.go` by exactly one line (the
  platform enum in the registration frame: `PLATFORM_INSTAGRAM` vs
  `PLATFORM_WHATSAPP`; verified by normalized diff at `15aa85f`). Key
  surface: `sendWithRetry` (client.go:223-244), constants
  `maxSendAttempts = 3`, `sendRetryDelay = 500ms` (client.go:16-21),
  `Permanent`/`isPermanent` (client.go:23-42), `Sender` interface
  (client.go:44-55: `SendMessage`, `EditMessage`, `DeleteMessage`).
- The WhatsApp test file to mirror,
  `connectors/whatsapp/internal/core/client_test.go` (69 lines): a
  `flakySender` stub counting calls, and three tests —
  `TestSendWithRetrySucceedsAfterTransientFailures`,
  `TestSendWithRetryGivesUpAfterMaxAttempts`,
  `TestSendWithRetryStopsOnContextCancel`. All drive
  `Client{logger: zerolog.Nop()}.sendWithRetry(...)` directly.
- WhatsApp's suite does not cover the permanent-error short-circuit
  (`isPermanent(err)` → immediate return, client.go:239-241); add that case
  for Instagram (and it will transfer to the shared package in plan 014).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `(cd connectors/instagram && go test ./...)` | exit 0; `internal/core` no longer reports "no test files" |
| Vet | `(cd connectors/instagram && go vet ./...)` | exit 0 |

## Scope

**In scope** (the only file you should create):
- `connectors/instagram/internal/core/client_test.go`

**Out of scope** (do NOT touch):
- `client.go` itself, and the telegram/whatsapp test files.

## Git workflow

- Branch: `advisor/013-instagram-client-tests`
- Commit style: Conventional Commits; suggested `test: cover the Instagram core client retry loop`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Port the WhatsApp test file

Copy `connectors/whatsapp/internal/core/client_test.go` to
`connectors/instagram/internal/core/client_test.go`. The `Sender` interfaces
are identical (neither has Telegram's `RetitleThread`), so the port is
mechanical; only the import path of the config package could differ — the test
imports no config, so likely zero changes are needed beyond the copy.

**Verify**: `(cd connectors/instagram && go test ./internal/core/)` → 3 tests
pass.

### Step 2: Add the permanent-error short-circuit case

Add:

```go
func TestSendWithRetryStopsImmediatelyOnPermanentError(t *testing.T) {
	t.Parallel()
	sender := &permanentSender{}
	_, _, err := Client{logger: zerolog.Nop()}.sendWithRetry(context.Background(), "d1", sendOpFor(sender))
	if !isPermanent(err) {
		t.Fatalf("err = %v, want permanent", err)
	}
	if sender.calls != 1 {
		t.Fatalf("calls = %d, want 1 (no retry on permanent errors)", sender.calls)
	}
}
```

with a `permanentSender` whose `SendMessage` returns
`Permanent(errors.New("invalid target"))` and counts calls.

**Verify**: `(cd connectors/instagram && go test ./internal/core/)` → 4 tests
pass.

## Test plan

The four tests above; structural pattern is the WhatsApp file verbatim.

## Done criteria

- [ ] `connectors/instagram/internal/core/client_test.go` exists with ≥4 tests
- [ ] `(cd connectors/instagram && go test ./... && go vet ./...)` exit 0
- [ ] No other file modified
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- Instagram's `client.go` no longer matches WhatsApp's modulo the platform
  line (plan 014 may have landed first — then this plan is REJECTED/stale;
  the shared package owns the tests).
- `sendWithRetry`'s signature differs from the excerpt.

## Maintenance notes

- Plan 014 moves these tests (and the WhatsApp/Telegram ones) into the shared
  client package; this file's content is its seed. The permanent-error case
  added here is the one gap in the existing suites — make sure it survives the
  move.
