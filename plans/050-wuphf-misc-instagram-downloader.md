# Plan 050: wuphf-misc — a feature host, with an Instagram reel/post downloader

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Build limits (the owner's laptop)**: prefix every cargo command with
> `nice -n 19` and set `CARGO_BUILD_JOBS=2`; Go with `GOMAXPROCS=2
> GOFLAGS=-p=2 nice -n 19`, one module at a time. Check and test only the
> crates and modules a step touches; run `cargo test --workspace` and clippy
> once, at the end. No release builds.
>
> **Drift check (run first)**: plan 011 must be DONE (Step 1 relies on its
> download size cap). Then `git diff --stat d5bb98a..HEAD -- crates/wuphf-proto
> crates/wuphf-model crates/wuphf-core/src/send.rs crates/wuphf-runtime/src
> crates/wuphf-tui connectors/instagram connectors/shared/functions`. On any
> change, compare the "Current state" excerpts with the live code; a mismatch
> is a STOP condition.

## Status

- **Priority**: P3
- **Effort**: L
- **Risk**: MED — a new process that posts into real chats on the owner's
  account; a core change to owner sends and to the observation log
- **Depends on**: 011 (Instagram download size cap)
- **Category**: feature
- **Planned at**: commit `d5bb98a`, 2026-11-20

## Why this matters

The owner wants features that are not bridging: the first is "when someone
posts an Instagram reel or post link in one of these chats, post the video or
photos there". Such features do not belong in the core (identity, routing,
dedup, loop prevention) nor in connectors (thin, no bridge decisions —
`AGENTS.md`). wuphf-misc is a separate process that hosts them, built only on
the core's existing client services, so the core stays a broker and every new
feature is a new module of wuphf-misc rather than new core logic.

## Design decisions (settled with the owner, 2026-11-20 — do not revisit)

| # | Decision |
|---|---|
| 1 | wuphf-misc is a Go process in `misc/` (its own module, using `connectors/shared`), binary `wuphf-misc`. It is a **client** of the core's existing services — `ObserverTransport` (`SubscribeMessages`, `SendMessage`), `FunctionTransport` (`Invoke`). No new peer kind in the core. |
| 2 | Features sit behind one small Go interface, each with its own config section and an `enabled` switch. No dynamic loading (no Go plugins, no scripting). The Instagram downloader is the first feature. |
| 3 | Config is wuphf-misc's own TOML file only (`misc.toml`); a change needs a restart. Panel settings wait for a later plugin-peer plan. |
| 4 | It authenticates with the **observer token** (full read/send access; documented as such). Scoped plugin tokens are a later plan. If the core's `functions.invoke_token_env` is set, wuphf-misc also reads that token. |
| 5 | The Instagram connector serves `instagram.fetch_media@1` through the function broker, with its own logged-in session. |
| 6 | Supported links: reels (`/reel/<code>`, `/reels/<code>`) and posts (`/p/<code>`), carousels included. Not stories, not profiles. |
| 7 | Trigger chats: a list of `connector:chat#thread` refs (the `notify` / rules form), on any connector. |
| 8 | A message triggers when it is **incoming** (the owner's own messages from the official apps included) or **outgoing** (TUI sends included), except outgoing rows whose `sent_by` is `"wuphf-misc"`. Edits never trigger. |
| 9 | `SendMessageRequest` gains an optional `client` field; the core stores it on the outgoing observed row as `ObservedMessage.sent_by` (absent when empty: no migration) and `SubscribeMessages` carries it. The TUI sets `"wuphf-tui"`. |
| 10 | `SendMessageRequest` and `OwnerMessage` gain attachments, given as **media ids**: the core loads each stored attachment (an unknown id is `INVALID_ARGUMENT`). Text may be empty when attachments are present. The reply is an owner message replying to the link message, and the existing fan-out to the chat's mirrors stays. |
| 11 | The reply is the media plus a short caption: the author and up to 200 characters of the post text; a switch turns the caption off. Links in the caption are **not** removed. |
| 12 | A failed fetch is only logged; a switch enables a short error reply in the chat. |
| 13 | Live messages only: links sent while wuphf-misc is down are not processed. |
| 14 | Same post in the same chat within 24 h → no second reply (in memory). At most 10 fetches per minute overall (extra links skipped and logged); at most 2 fetches at a time. All configurable. |

## Current state

- **Observer service** (`crates/wuphf-proto/observer.proto`):
  `SendMessageRequest { connector_id = 1; chat_id = 2; optional thread_id = 3;
  text = 4; optional reply_to_platform_message_id = 5; }`;
  `ObservedMessage` fields 1–15 (`direction = 12`, `deleted = 14`, `edited =
  15`). Handler: `crates/wuphf-runtime/src/observer_service.rs`
  `send_message` (~line 144): refuses an empty `text`, builds an
  `OwnerMessage`, waits up to `SEND_ACK_TIMEOUT` for the native id.
- **Owner send**: `wuphf_model::OwnerMessage` (`dispatch.rs`: `connector_id,
  chat_id, thread_id, text, reply_to_native_id, timestamp_ms`);
  `Core::send_owner_message` (`crates/wuphf-core/src/send.rs`) builds a
  `Message` with `attachments: Vec::new()`, a direct dispatch (unlabelled) and
  the mirror fan-out through `route_and_record`.
  `OwnerMessageSender::send` / `observed_template`
  (`crates/wuphf-runtime/src/owner_send.rs`) builds the outgoing observed row
  and tracks it until the acknowledgement.
- **Observation**: `ObservedMessage` (`crates/wuphf-model/src/observation.rs`),
  stored as JSON in `observed_messages`. An echo of an owner message keeps
  its outgoing row (`observation.rs`, `observe`).
- **TUI**: `crates/wuphf-tui/src/client.rs` `send_message` builds
  `pb::SendMessageRequest`.
- **Function SDK (Go)**: `connectors/shared/functions` — provider side
  `Serve(ctx, ProviderOptions{Address, ProviderID, Token, Functions}, logger)`
  with `Function{Name, Major, RequestSchema, ResponseSchema, Idempotent,
  MaxConcurrency, DefaultDeadline, Handle}`; caller side `Dial(address, token,
  caller)` and `Client.Invoke(ctx, name, major, args, mediaIDs, &result)
  ([]string, error)`. `providers/mediaconv` is the reference provider.
- **Instagram fetch**: `connectors/instagram/internal/instagram/media.go` —
  `fetchShareURL` uses `FetchMedia(ctx, mediaID, shortcode)` (instameow,
  `GET media_info/{mediaID}`) and picks the largest video, else image;
  `fetch` downloads and uploads through the `MediaBroker`. The response has
  `CarouselMedia []*Items`. `FetchMedia` needs the numeric media id: a link
  gives a shortcode, which decodes to the id (base64 alphabet
  `A-Za-z0-9-_`, big-endian, 6 bits per character).
- **Function config in the core**: `[[functions.providers]] id, token_env,
  functions` (`config/example.toml`, mediaconv's entry).
- **Deploy**: `scripts/dev-bin/wuphf-*`, `scripts/dev-layout.kdl`,
  `scripts/install-dev-bins.sh`, `deploy/systemd/` (one unit per part).

## Steps

### Step 1: `instagram.fetch_media@1` in the Instagram connector

`connectors/instagram`:

- **Config** (`internal/config`): an optional `[functions]` section —
  `provider_id` (e.g. `"instagram-main"`), `token_env`, `max_concurrency`
  (default 2). Absent → the connector serves no function (behavior as today).
- **Link parsing** (new `internal/instagram/links.go`, pure): `ParsePostLink(url)
  (shortcode string, ok bool)` for `instagram.com/(p|reel|reels)/<code>`
  (with or without `www.`, query and trailing slash ignored);
  `MediaIDFromShortcode(code) (string, error)`.
- **Handler** (new `internal/instagram/fetchfunc.go`): request `{ "url":
  string }`; response `{ "items": [{ "kind": "video"|"image" }], "author":
  string, "caption": string, "skipped": int }`, the uploaded media ids
  returned in the result's media-id list in item order. For a carousel, one
  item per `CarouselMedia` entry; each item takes its largest video, else its
  largest image (the existing selection, factored out of `fetchShareURL`).
  Download each through plan 011's capped fetch; an item over the cap, or
  over the core's upload limit (the upload fails), is left out and counted in
  `skipped`. Errors: a URL that is not a post/reel link →
  `functions.InvalidArgument`; `FetchMedia` failure or no items → an error
  the caller sees as a failed call. Mark the function `Idempotent`.
- **Wiring** (`connector.go`): when `[functions]` is set, run
  `functions.Serve` next to the connector session, sharing the logged-in
  instameow client and the media broker; it stops with the connector.
- Core config example: a commented `[[functions.providers]] id =
  "instagram-main", token_env = "WUPHF_PROVIDER_INSTAGRAM_TOKEN", functions =
  ["instagram.fetch_media"]`.

Tests: `ParsePostLink` table (reel, reels, p, www, query, trailing slash,
stories → no, profile → no, other host → no); `MediaIDFromShortcode` against
known pairs; the handler against a fake fetcher and broker: a reel (video),
a photo post, a carousel of three (order kept), an oversized item
(`skipped: 1`), a fetch failure.

**Verify**: `connectors/instagram` `go vet ./...`, `go test ./...` → green.
Commit: `feat(instagram): serve instagram.fetch_media@1`.

### Step 2: The core — attachments and `client` on owner sends

- **Proto**: `SendMessageRequest` gains `repeated string media_ids = 6;
  optional string client = 7;`. `ObservedMessage` gains `optional string
  sent_by = 16;`. Regenerate only `observer.pb.go` (the local protoc rewrites
  headers of untouched files).
- **Model**: `OwnerMessage` gains `media_ids: Vec<MediaId>` and `client:
  Option<String>`; `ObservedMessage` gains `sent_by: Option<String>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]` — existing
  rows keep their JSON, no migration).
- **Core** (`send.rs`): `send_owner_message` loads each media id
  (`storage.media(id)`; unknown → a new `CoreError::UnknownMedia`, already
  defined) and puts the stored attachments on the `Message`, so the direct
  copy and the mirrors carry them.
- **Runtime**: `observer_service.rs` `send_message` accepts an empty `text`
  when `media_ids` is non-empty (both empty → `INVALID_ARGUMENT`), parses the
  ids (invalid → `INVALID_ARGUMENT`), maps `CoreError::UnknownMedia` to
  `INVALID_ARGUMENT`, and passes `client`. `owner_send.rs`
  `observed_template` sets `attachments` and `sent_by`; the echo path must
  keep `sent_by` (it keeps the outgoing row already — assert it). The
  observer's proto mapping carries `sent_by`.
- **TUI**: `client.rs` sets `client: Some("wuphf-tui")`; `media_ids` empty.

Tests: core — an owner message with two media ids reaches the direct dispatch
and the mirror dispatch with both attachments; an unknown id fails. Runtime —
`send_message` with only media ids succeeds, with neither fails; the
outgoing observed row (and its echo) carries `sent_by`; `SubscribeMessages`
streams `sent_by`. Storage — a stored `ObservedMessage` without `sent_by`
decodes and re-serializes byte for byte (`migration_tests.rs` style).

**Verify**: `cargo test -p wuphf-model -p wuphf-storage -p wuphf-core -p
wuphf-runtime -p wuphf-tui` → green; `proto/go` build + vet.
Commit: `feat(observer): attachments and a client tag on owner sends`.

### Step 3: The wuphf-misc skeleton

New Go module `misc/` (`go.mod` with the same `replace` lines as the
connectors for `proto/go` and `connectors/shared`), `cmd/wuphf-misc`,
`internal/config`, `internal/misc`.

- **Config** (`misc.toml`, `config.example.toml`): `core_address`,
  `observer_token_env`, optional `invoke_token_env`, `log_level`, and one
  table per feature (`[instagram]` in Step 4).
- **Feature interface** (`internal/misc/feature.go`):
  ```go
  type Message struct { /* the ObservedMessage fields a feature needs, incl. Direction, SentBy */ }
  type Feature interface {
      Name() string
      Handle(ctx context.Context, message Message, out Sender) // never blocks the stream
  }
  type Sender interface { // the only way a feature posts
      Reply(ctx context.Context, to Message, text string, mediaIDs []string) error
  }
  ```
  `Reply` calls `SendMessage` to the message's own chat and thread, replying
  to its native id, with `client: "wuphf-misc"`.
- **Runner** (`internal/misc/run.go`): one `SubscribeMessages` stream (no
  filter), reconnecting with backoff; per message: drop edits (`edited`),
  drop deleted, drop outgoing rows with `sent_by == "wuphf-misc"`, then hand
  the message to every enabled feature. A feature's work runs in its own
  goroutine so a slow fetch never stalls the stream; the runner waits for
  them on shutdown (SIGINT).

Tests: the filter (incoming triggers; outgoing from the TUI triggers;
outgoing with `sent_by = "wuphf-misc"` does not; edits do not); the runner
against a fake observer stream delivers to enabled features only and
reconnects after the stream ends.

**Verify**: `misc` `go vet ./...`, `go test ./...` → green.
Commit: `feat(misc): wuphf-misc, a host for non-bridging features`.

### Step 4: The Instagram downloader feature

`internal/misc/instagram` with config:

```toml
[instagram]
enabled = true
chats = ["whatsapp-main:120363000000000000@g.us"]
caption = true            # author + up to 200 characters of the post text
reply_on_failure = false
dedup_hours = 24
max_per_minute = 10
max_concurrent = 2
call_timeout_seconds = 60
```

- A message triggers when its `connector:chat#thread` is in `chats` (an
  entry without a thread matches the whole chat) and its text contains one
  or more post/reel links (the same parser as Step 1, copied or shared —
  not imported from the connector module).
- Each link: skip if `(chat, shortcode)` was answered within `dedup_hours`;
  skip and log if over `max_per_minute`; else, under the `max_concurrent`
  semaphore, `Invoke("instagram.fetch_media", 1, {url}, nil, &result)` with
  `call_timeout_seconds`, then `Reply(message, caption, mediaIDs)`. Caption:
  `"@<author>: <first 200 characters of the caption>"` (cut at a rune
  boundary, `…` when cut), or empty when `caption = false`. No items → treat
  as a failure. Failure → log; when `reply_on_failure`, reply with
  `"Could not download this post."`.
- Record the dedup entry only after a successful reply.

Tests (fake function client and fake sender): a configured chat with a reel
link → one invoke, one reply with the media ids and the caption; an
unconfigured chat → nothing; the same link twice → one reply; a carousel →
one reply with all media; the rate limit skips the eleventh link in a
minute; a failure with and without `reply_on_failure`; `caption = false` →
empty text; a caption over 200 characters is cut.

**Verify**: `misc` `go test ./...` → green.
Commit: `feat(misc): post Instagram reels and posts linked in configured chats`.

### Step 5: Deployment and docs

- `scripts/dev-bin/wuphf-misc` (like `wuphf-mediaconv`, config from
  `$WUPHF_MISC_CONFIG`), a tab in `scripts/dev-layout.kdl`, an entry in
  `scripts/install-dev-bins.sh`, the alias list in `dev-aliases.sh`.
- `deploy/systemd/wuphf-misc.service` (pattern of `wuphf-mediaconv.service`,
  `KillSignal=SIGINT`, `MemoryDenyWriteExecute=yes`) and
  `env/misc.env.example` (`WUPHF_OBSERVER_TOKEN`, optional invoke token);
  the instagram unit's env example gains `WUPHF_PROVIDER_INSTAGRAM_TOKEN`.
  Update `deploy/systemd/README.md`.
- Docs: `README.md` (one bullet, the layout list), `docs/architecture.md`
  (wuphf-misc as a client of the observer and function services; the
  `sent_by` rule), `docs/connector-protocol.md` (`SendMessage` media ids and
  `client`, `ObservedMessage.sent_by`), `misc/README.md` (what it does, the
  observer token is full access, Instagram account risk).

Commit: `docs: wuphf-misc`.

### Step 6: Gate and index

`cargo test --workspace`, clippy and fmt clean; Go `proto/go`, `shared`,
`instagram`, `misc`, `telegram`, `whatsapp` green (`mediaconv`'s animated-WebP
tests fail on machines whose ffmpeg lacks `libwebp_anim` — environment).
Row 050 DONE with notes. Commit: `docs(plans): mark plan 050 done`.

## Test plan

| Test | Where | Proves |
|---|---|---|
| link parsing, shortcode → id, handler (reel, photo, carousel, oversize, failure) | instagram | decisions 5, 6 |
| owner send with media ids, unknown id, `sent_by` on the row and its echo, old rows decode | core, runtime, storage | decisions 9, 10 |
| trigger filter, runner delivery and reconnect | misc | decisions 1, 2, 8, 13 |
| configured chats, dedup, rate limit, carousel, caption, failure reply | misc | decisions 7, 11, 12, 14 |

## STOP conditions

- Plan 011 is not DONE.
- `FetchMedia` (or the shortcode → id decoding) cannot fetch a reel or post
  by link with the connector's session: report what the API returns.
- The function broker refuses a provider that is also a connector process
  (a second session from the same binary) — report; do not merge it into the
  connector session.
- Accepting media ids in `SendMessage` would let a caller attach media it
  did not upload and the core has no way to tell — report rather than add an
  ownership model (owner-only access through the observer token is the v1
  assumption).
- An existing migration fixture or observed-row test fails after adding
  `sent_by` (a new field serializes when empty).

## Maintenance notes

- **Loop safety** rests on decision 8: wuphf-misc's own replies carry
  `sent_by = "wuphf-misc"` and never trigger it. Captions keep links (decision
  11), so a caption linking another post does not trigger either — but only
  because of `sent_by`; never drop that filter.
- **Account risk.** Every fetch runs on the Instagram connector's account;
  keep `max_per_minute` low.
- **Later plans**: scoped plugin tokens and a chat allow-list in the core
  (`[[plugins]]`), panel settings for wuphf-misc's features, catch-up after a
  restart, more link sources.
