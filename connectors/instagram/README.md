# Instagram Connector

This adapter uses [`mautrix-meta`](https://github.com/mautrix/meta)'s
`instameow` client and the shared gRPC contract under `proto/go`. (Upstream
dropped Instagram from the older `messagix` lightspeed client after v26.07;
see `plans/048-instagram-instameow-port.md`.)

Implemented:

- cookie-based authentication: session cookies are read from a configured JSON
  file and refreshed cookies are persisted back to a session file across restarts
- inbound messages from the realtime (DGW) delta stream, keyed by Instagram
  thread keys (decimal IDs): text, media (photos, videos, albums, voice notes,
  GIFs, stickers), shared posts/reels (full quality when resolvable), edits,
  unsends, reactions, and chat titles
- outbound text (with reply target), edits, unsends and reactions over
  Instagram's graphql API, resolving the canonical message id from the send
  response
- offline catch-up: the stream position is saved to `state_file`, and a
  restart resumes from it, replaying what was missed

Deferred connector work:

- outbound attachments
- read receipts
- end-to-end live integration tests

Routing, identity, deduplication, echo suppression, reply mapping, and media
retention remain in Rust core. The connector is a raw pipe: it emits Instagram
events tagged with their native thread key and executes dispatches addressed by
thread key, owning no mapping itself. Instagram direct messages have no topics,
so this connector declares no capabilities and is never used as a threaded
mirror. It intentionally submits messages authored by the logged-in account;
core decides whether each is user input or a bridge echo.

## Configuration

Copy `config.example.toml`. Export your logged-in Instagram session cookies to
the JSON file named by `cookies_file` — at minimum `sessionid`, `csrftoken`,
`ds_user_id`, `mid`, and `ig_did`. The connector writes the latest refreshed
cookies to `session_file` on shutdown and prefers it on the next start, so
when replacing cookies, stop the connector and move `session_file` aside too.

The connector owns no routing; configure which conversations map where in the
core's `[rules.<id>]` entries using Instagram thread keys (decimal numbers) as
chat IDs.

The connector authenticates to the core with the token in the variable
`core_token_env` names, which must hold the same value the core reads from this
connector's `[[connectors]].token_env`.

Run core server, then connector:

```sh
export WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN=replace-me
go run ./cmd/wuphf-instagram --config config.example.toml
```

The keys marked "Runtime." in `config.example.toml` can also change while the
connector runs, from the core's settings panel or `wuphf settings`: the
connector declares them to the core and applies each change the core pushes
(see `docs/connector-protocol.md`, "A connector's own settings").
