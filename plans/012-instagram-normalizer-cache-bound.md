# Plan 012: Bound the Instagram normalizer's message→thread cache

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 15aa85f..HEAD -- connectors/instagram/internal/instagram/updates.go`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S-M
- **Risk**: LOW (worst case: an edit of a very old message is skipped — which
  is already the behavior for any message seen before connector startup)
- **Depends on**: none
- **Category**: bug (slow memory leak)
- **Planned at**: commit `15aa85f`, 2026-06-11

## Why this matters

The Instagram `Normalizer` records **every** message id → thread key mapping
it ever sees in a plain map and never evicts
(`recordMessageThread`, called per inbound message). On a long-running
connector with busy chats this grows without bound — a slow leak measured in
tens of bytes per message, forever. The map exists only so *edits* (whose
`LSEditMessage` rows carry no thread key) can be resolved; edits in practice
target recent messages. A bounded LRU keeps the hot window and caps memory.

The other two caches (`threadNames`, `contactNames`) grow with the number of
distinct threads/contacts — bounded by the account's social graph, not by
traffic — and are deliberately left alone.

## Current state

- `connectors/instagram/internal/instagram/updates.go:33-37` — the caches:

```go
	mu             sync.Mutex
	threadNames    map[int64]string
	contactNames   map[int64]string
	messageThreads map[string]int64
```

- Writer: `recordMessageThread` (updates.go:238-242) — called for every
  `LSInsertMessage` in `handleCreates` (updates.go:103).
- Sole reader: `messageThread` (updates.go:244-249), used by `handleEdits`
  (updates.go:166-186); a cache miss already has a graceful path:

```go
		threadKey, ok := n.messageThread(edit.MessageID)
		if !ok {
			n.logger.Warn().Str("message_id", edit.MessageID).Msg("skipping Instagram edit for unknown message")
			continue
		}
```

- Constructor: `NewNormalizer` (updates.go:39-49) initializes the maps.
- Existing tests: `connectors/instagram/internal/instagram/updates_test.go`
  (366 lines) — exercises `Handle` with synthetic `table.LSTable` deltas;
  model new tests on it.
- Dependencies: `connectors/instagram/go.mod` — check whether
  `github.com/hashicorp/golang-lru/v2` is already an indirect dep before
  adding it.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build/vet/tests | `(cd connectors/instagram && go build ./... && go vet ./... && go test ./...)` | exit 0 |
| Dep tidy | `(cd connectors/instagram && go mod tidy)` | go.mod/go.sum updated only with the LRU dep |

## Scope

**In scope** (the only files you should modify):
- `connectors/instagram/internal/instagram/updates.go`
- `connectors/instagram/internal/instagram/updates_test.go`
- `connectors/instagram/go.mod` / `go.sum` (LRU dependency, if used)

**Out of scope** (do NOT touch):
- `threadNames` / `contactNames` (bounded by social graph; eviction would
  break title resolution for quiet threads).
- Telegram/WhatsApp connectors (their edit events carry chat ids natively;
  verify nothing similar exists there before assuming — but do not change
  them in this plan).
- Persisting the cache across restarts.

## Git workflow

- Branch: `advisor/012-instagram-cache-bound`
- Commit style: Conventional Commits; suggested `fix: bound the Instagram message-to-thread cache`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Swap the map for a bounded LRU

Preferred: `github.com/hashicorp/golang-lru/v2` (`lru.New[string, int64](messageThreadCacheSize)`),
with:

```go
// messageThreadCacheSize bounds the message->thread cache used to resolve
// edits (LSEditMessage carries no thread key). Edits target recent messages;
// an eviction behaves exactly like a pre-startup message: the edit is skipped.
const messageThreadCacheSize = 8192
```

The LRU type is internally synchronized — `recordMessageThread` and
`messageThread` no longer need `n.mu` for this cache (keep `mu` for the two
name maps). If adding the dependency is undesirable, a fixed-size
two-generation map (swap when the active generation reaches the cap, read
checks both) is an acceptable stdlib-only fallback — pick ONE, do not build a
custom LRU with linked lists.

Update `NewNormalizer`, `recordMessageThread`, `messageThread` accordingly.

**Verify**: `(cd connectors/instagram && go build ./... && go vet ./...)` →
exit 0; `go mod tidy` adds only the LRU module (if used).

### Step 2: Tests

In `updates_test.go`, following its existing delta-construction style:

1. `editResolvesThreadThroughCache` (likely exists in some form — if so, just
   confirm it still passes): insert message, then edit → edit event carries
   the right chat id.
2. `editOfEvictedMessageIsSkipped`: fill the cache past
   `messageThreadCacheSize` with synthetic inserts (loop constructing
   `LSInsertMessage` rows; keep the count small by temporarily... NO — the
   const is not configurable; instead make the cache size a `Normalizer` field
   defaulting to the const, settable in tests via a small constructor option
   or direct field assignment before use). Then edit the first message id →
   no frame emitted, and the run does not error.

Keep the test cache size tiny (e.g. 4) via the field/option so the test stays
fast.

**Verify**: `(cd connectors/instagram && go test ./...)` → exit 0.

## Test plan

The two tests above; pattern = existing `updates_test.go` table-delta
construction. Full connector suite green.

## Done criteria

- [ ] `grep -n 'messageThreads map' connectors/instagram/internal/instagram/updates.go` → no matches
- [ ] Cache bound constant exists and is documented with the eviction trade-off
- [ ] `(cd connectors/instagram && go build ./... && go vet ./... && go test ./...)` exit 0
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- `messageThreads` has gained additional readers since `15aa85f` (eviction
  might then lose more than edit resolution — reassess).
- Deletes also resolve through the cache (at `15aa85f` `handleDeletes` uses
  `del.ThreadKey` directly, updates.go:188-198 — if that changed, eviction
  affects deletes too; report).

## Maintenance notes

- If users report missed edits on old messages, raise
  `messageThreadCacheSize` — entries cost ~50 bytes each, so even 100k is
  ~5 MB. The cap exists to bound *unbounded* growth, not to be tight.
- The two name caches are next if memory profiling ever flags them; they need
  a different policy (they are lookups by social-graph key, not by message).
