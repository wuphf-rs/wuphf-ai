# Instagram Connector

This adapter uses [`mautrix-meta`](https://github.com/mautrix/meta)'s `messagix`
client and the shared gRPC contract under `proto/go`.

Implemented slice (MVP):

- cookie-based authentication: session cookies are read from a configured JSON
  file and refreshed cookies are persisted back to a session file across restarts
- raw-pipe inbound text replication keyed by Instagram thread keys (decimal IDs)
- outbound text replication via the lightspeed task socket, resolving the
  canonical message id from the send response
- inbound reply context carried through native Instagram message IDs

Deferred connector work:

- media (images, video, audio, attachments)
- edits, deletes, reactions, and thread titles
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
cookies to `session_file` on shutdown and prefers it on the next start.

The connector owns no routing; configure which conversations map where in the
core's `[[rules]]` using Instagram thread keys (decimal numbers) as chat IDs.

The connector authenticates to the core with the token in the variable
`core_token_env` names, which must hold the same value the core reads from this
connector's `[[connectors]].token_env`.

Run core server, then connector:

```sh
export WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN=replace-me
go run ./cmd/wuphf-instagram --config config.example.toml
```
