# Plan 011: Cap Instagram CDN downloads instead of buffering unbounded bodies

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- connectors/instagram/internal/instagram/media.go`
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

The Instagram connector downloads attachment bytes from CDN URLs with
`io.ReadAll(response.Body)` — no size limit. The core *does* enforce
`max_media_bytes` (default 32 MiB, `crates/wuphf-cli/src/main.rs:46-48`,
checked chunk-by-chunk in `crates/wuphf-runtime/src/media.rs:70-73`), but
only at upload time — **after** the connector has already buffered the entire
body in RAM. A huge or maliciously slow-and-endless CDN response (the URL
comes from message content) can OOM the connector process. The fix is a
client-side cap before buffering.

## Current state

- `connectors/instagram/internal/instagram/media.go:80-105` — the fetch:

```go
func (m MediaCodec) fetch(ctx context.Context, ref mediaRef) (*wuphfpb.Attachment, error) {
	...
	response, err := m.http.Do(request)
	...
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("download Instagram media: unexpected status %s", response.Status)
	}
	bytes, err := io.ReadAll(response.Body)
	if err != nil {
		return nil, fmt.Errorf("read Instagram media body: %w", err)
	}
	attachment, err := m.broker.Upload(ctx, ref.metadata, bytes)
	...
```

- `HTTPDoer` is an interface (`media.go:23-25`) — tests stub the CDN, so the
  cap is unit-testable without network.
- Failure handling upstream: a fetch error makes `fetchAll` fail
  (`media.go:68-78`), and the caller logs and bridges the message **text-only**
  (`connectors/instagram/internal/instagram/updates.go:123-131` — deliberate
  policy, comment on lines 125-126). An oversize attachment therefore degrades
  gracefully; no new handling needed.
- Existing instagram tests: `connectors/instagram/internal/instagram/`
  has `updates_test.go`, `sender_test.go` — **no media_test.go yet**. Test
  style: standard `testing`, stub interfaces (see `sender_test.go`).
- The Telegram/WhatsApp connectors buffer platform downloads too, but those
  bytes come through authenticated platform APIs with platform-enforced size
  ceilings, not arbitrary HTTP URLs — deliberately out of scope.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `(cd connectors/instagram && go build ./...)` | exit 0 |
| Tests | `(cd connectors/instagram && go test ./...)` | exit 0 |
| Vet | `(cd connectors/instagram && go vet ./...)` | exit 0 |

## Scope

**In scope** (the only files you should create/modify):
- `connectors/instagram/internal/instagram/media.go`
- `connectors/instagram/internal/instagram/media_test.go` (create)

**Out of scope** (do NOT touch):
- Telegram/WhatsApp media paths (platform-capped; separate decision).
- `connectors/instagram/internal/core/media.go` (downloads from the core,
  whose stored blobs already passed the core's limit).
- Making the cap configurable (constant first; see Maintenance notes).
- The text-only degradation policy in `updates.go`.

## Git workflow

- Branch: `advisor/011-instagram-media-cap`
- Commit style: Conventional Commits; suggested `fix: cap Instagram CDN downloads at 64 MiB`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add the cap

In `media.go`, add:

```go
// maxDownloadBytes caps a CDN media download. The core rejects anything over
// its configured max_media_bytes at upload anyway (32 MiB by default); this
// bound only prevents buffering an unbounded response body before that check.
const maxDownloadBytes = 64 * 1024 * 1024
```

and replace the `io.ReadAll(response.Body)` call with a limited read that
distinguishes "hit the cap" from EOF:

```go
	bytes, err := io.ReadAll(io.LimitReader(response.Body, maxDownloadBytes+1))
	if err != nil {
		return nil, fmt.Errorf("read Instagram media body: %w", err)
	}
	if len(bytes) > maxDownloadBytes {
		return nil, fmt.Errorf("download Instagram media: body exceeds %d bytes", maxDownloadBytes)
	}
```

**Verify**: `(cd connectors/instagram && go build ./... && go vet ./...)` →
exit 0.

### Step 2: Tests

Create `media_test.go` with a stub `HTTPDoer` returning an
`http.Response{Body: io.NopCloser(...)}`:

1. body under the cap → fetch succeeds, broker stub receives exactly the body
   bytes;
2. body over the cap (e.g. `io.NopCloser(io.LimitReader(neverEndingReader{},
   maxDownloadBytes+10))` or a `bytes.Repeat` buffer one byte over) → fetch
   returns an error containing "exceeds", and the broker stub is **not**
   called;
3. non-200 status → existing error path still works (regression guard).

Stub the `MediaBroker` interface (`media.go:18-20`) with a recording fake.

**Verify**: `(cd connectors/instagram && go test ./...)` → exit 0, 3 new tests
pass.

## Test plan

The three tests in Step 2; model interface-stubbing style after
`connectors/instagram/internal/instagram/sender_test.go`.

## Done criteria

- [ ] `grep -n 'io.ReadAll(response.Body)' connectors/instagram/internal/instagram/media.go` → no matches
- [ ] `grep -n maxDownloadBytes connectors/instagram/internal/instagram/media.go` → const + 2 uses
- [ ] `(cd connectors/instagram && go build ./... && go vet ./... && go test ./...)` → exit 0
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `fetch` was refactored to stream directly to the broker (drift — the cap
  then belongs in the streaming loop, a different change).
- The broker `Upload` signature no longer takes `[]byte` (streaming upload
  landed); the right fix changes shape — report.

## Maintenance notes

- If the core's `max_media_bytes` is raised above 64 MiB, raise
  `maxDownloadBytes` too (or make it a config field then — the config plumbing
  is `connectors/instagram/internal/config`).
- A later improvement: send `Range`/`Content-Length` preflight to reject early
  without reading; not worth it until oversize media is actually observed.
