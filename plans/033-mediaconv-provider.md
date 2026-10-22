# Plan 033: Move sticker conversion into a `media.convert` provider

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 2c92f5a..HEAD -- connectors/mediaconv connectors/shared connectors/telegram connectors/whatsapp flake.nix`
> Plan 032 must be DONE: `test -f proto/go/connector/v1/functions_grpc.pb.go`
> → exit 0 and `grep -q "FunctionTransportClient" proto/go/connector/v1/functions_grpc.pb.go`
> → exit 0. If any in-scope connector file changed since `2c92f5a`, compare the
> "Current state" excerpts against the live code; on a mismatch, STOP.

## Status

- **Priority**: P2
- **Effort**: M–L
- **Risk**: MED — changes how animated stickers are delivered on Telegram and
  WhatsApp; a missing provider degrades them to the existing fallbacks
- **Depends on**: plans/032-remote-function-broker.md (hard)
- **Category**: direction
- **Planned at**: commit `2c92f5a`, 2026-10-22

## Why this matters

Animated-sticker conversion is compiled into two Go connectors: Telegram calls
`mediaconv.AnimatedWebPToWebM`, and WhatsApp calls `mediaconv.ToAnimatedWebP`
or a cgo Lottie renderer (`tgsconverter`/`rlottie`, C++). Any connector in
another language must re-implement all of it, and the WhatsApp connector
carries a C++ build only for stickers. Plan 032 added a function broker to the
core. This plan moves the conversions into the first provider, a small Go
process serving `media.convert@1`, adds a Go SDK for calling and serving
functions, and switches both connectors to call it. Local conversion is
**removed**, by the owner's decision, so the conversions exist exactly once.
The provider is in Go as an MVP; it can be rewritten in another language later
without touching the connectors, since they only see the `media.convert@1`
contract.

## Current state

- `connectors/mediaconv/` — its own Go module
  (`github.com/akshettrj/wuphf/connectors/mediaconv`, no dependencies) with
  `mediaconv.go` and `mediaconv_test.go`. Exports
  `AnimatedWebPToWebM(ctx, src []byte) ([]byte, error)`,
  `ToAnimatedWebP(ctx, src []byte, srcExt string) ([]byte, error)` and
  `*MissingToolError`. Shells out to `ffmpeg` and ImageMagick. Its tests skip
  when the tools are missing and define `isAnimatedWebP` (line 141).
- `connectors/whatsapp/internal/whatsapp/lottie.go` —
  `func tgsToAnimatedWebP(tgs []byte) ([]byte, error)` using
  `github.com/watgbridge/tgsconverter/libtgsconverter` (cgo, rlottie).
  `lottie_test.go` has `TestTGSToAnimatedWebP`, `sampleTGS` and **another**
  `isAnimatedWebP` (line 47) — a duplicate of the mediaconv one.
- Telegram, `connectors/telegram/internal/telegram/sender.go`:

  ```go
  // sender.go:22
  type Sender struct {
      api    *tg.Client
      sender *message.Sender
      peers  *PeerCache
      media  MediaBroker
      rendering config.RenderingOptions
  }
  func NewSender(api *tg.Client, peers *PeerCache, media MediaBroker, rendering config.RenderingOptions) Sender

  // sender.go:510-536
  func (s Sender) sendSticker(ctx, builder, attachment, data) (tg.UpdatesClass, error) {
      if attachment.GetAnimated() {
          if webm, err := s.animatedStickerWebM(ctx, attachment, data); err == nil {
              … upload as video sticker …
          }
          // Fall through: deliver the original as a static sticker (first frame).
      }
      return builder.Upload(message.FromBytes("sticker.webp", data)).Sticker(ctx)
  }
  func (s Sender) animatedStickerWebM(ctx, attachment *wuphfpb.Attachment, data []byte) ([]byte, error) {
      if attachment.GetMimeType() == "video/webm" {
          return data, nil
      }
      return mediaconv.AnimatedWebPToWebM(ctx, data)
  }
  ```
  Wiring in `connectors/telegram/internal/telegram/connector.go:33-67`:
  `media, err := coreclient.DialMedia(c.cfg.CoreAddress)` … `sender :=
  NewSender(client.API(), peers, media, c.cfg.Rendering)`.

- WhatsApp, `connectors/whatsapp/internal/whatsapp/sender.go`:

  ```go
  // sender.go:27
  type Sender struct {
      client      SendClient
      media       MediaBroker
      ownJID      types.JID
      stickerMeta config.StickerMetadata
      rendering   config.RenderingOptions
  }
  func NewSender(client SendClient, media MediaBroker, ownJID types.JID, stickerMeta config.StickerMetadata, rendering config.RenderingOptions) Sender

  // sender.go:358-374
  func (s Sender) stickerMessage(ctx, attachment, data []byte, mimeType string) (*waE2E.Message, error) {
      if attachment.GetAnimated() && mimeType != "image/webp" {
          converted, err := animatedStickerWebP(ctx, attachment, data, mimeType)
          if err != nil {
              return s.documentMessage(ctx, data, mimeType, stickerFileName(attachment), "")
          }
          data, mimeType = converted, "image/webp"
      }
      …
  }
  // sender.go:394-404
  func animatedStickerWebP(ctx, attachment, data []byte, mimeType string) ([]byte, error) {
      if mimeType == "application/x-tgsticker" || stickerSourceExt(attachment) == ".tgs" {
          return tgsToAnimatedWebP(data)
      }
      return mediaconv.ToAnimatedWebP(ctx, data, stickerSourceExt(attachment))
  }
  // sender.go:433-445 — stickerSourceExt: "video/webm"→".webm",
  // "application/x-tgsticker"→".tgs", else path.Ext(file name), else ".bin"
  ```
  Wiring in `connectors/whatsapp/internal/whatsapp/connector.go:49-99`:
  `media, err := coreclient.DialMedia(c.cfg.CoreAddress)` …
  `NewSender(client, media, ownJID, c.cfg.StickerMetadata(), c.cfg.Rendering)`.
  `sender_test.go` calls `NewSender(…)` at about 13 sites.

- Both connectors' `go.mod` require `github.com/akshettrj/wuphf/connectors/mediaconv`
  through a `replace … => ../mediaconv` directive. WhatsApp also requires
  `github.com/watgbridge/tgsconverter` and indirectly `github.com/Benau/go_rlottie`.
  WhatsApp still needs cgo afterwards for `github.com/mattn/go-sqlite3`; only
  the C++ rlottie build leaves it.
- `connectors/shared` — Go module `github.com/akshettrj/wuphf/connectors/shared`
  with packages `coreclient` (session client; `media.go` has
  `DialMedia(address) (*MediaClient, error)`, `Upload(ctx, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error)`,
  `Download(ctx, mediaID string) ([]byte, error)`) and `logging`
  (`logging.New(configured string) zerolog.Logger`). Connectors import it via
  `replace github.com/akshettrj/wuphf/connectors/shared => ../shared`.
- Connector configs are TOML (`github.com/pelletier/go-toml/v2`) in
  `internal/config/config.go` with a `File` struct and `Validate()`; each
  connector has `config.example.toml` and `README.md`. Binaries live in
  `cmd/wuphf-<name>/main.go` (see `connectors/telegram/cmd/wuphf-telegram/main.go`).
- **Delivery is sequential per connector** (`connectors/shared/coreclient/client.go`,
  `runSession`'s `for … select` calls `c.deliver` inline). A conversion that
  waits blocks that connector's later dispatches for as long as it waits.
- Plan 032 (done before this) provides, in package `wuphfpb`
  (`proto/go/connector/v1`): `FunctionTransportClient` (`Invoke`,
  `ListFunctions`), `ProviderTransportClient` (`OpenProviderSession`),
  `InvokeRequest`, `FunctionRef`, `FunctionDeclaration`, `ProviderHello`,
  `ProviderClientFrame`, `ProviderServerFrame`, `ProviderCall`, `CancelCall`,
  `CallResult`, `CallSuccess`, `CallError`, `CallErrorCode_*`. Semantics
  (read `docs/connector-protocol.md` "Remote functions" before starting):
  a call to a name that no configured provider may serve fails at once with
  `NOT_FOUND`; a servable call waits for a provider until its deadline; a
  provider authenticates with `authorization: Bearer <token>` and its first
  frame is `ProviderHello`; `CancelCall` means stop work; a provider
  reconnecting with the same id takes over its old session.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Shared SDK | `cd connectors/shared && go build ./... && go vet ./... && go test ./...` | exit 0 |
| Provider | `cd providers/mediaconv && go build ./... && go vet ./... && go test ./...` | exit 0 (conversion tests skip without ffmpeg/ImageMagick) |
| Telegram | `cd connectors/telegram && go build ./... && go vet ./... && go test ./...` | exit 0 |
| WhatsApp | `cd connectors/whatsapp && go build ./... && go vet ./... && go test ./...` | exit 0 |
| Rust gate | `cargo test --workspace` | exit 0, count unchanged (this plan touches no Rust) |

Run inside `nix develop` (Go, ffmpeg, ImageMagick, a C/C++ toolchain).
Baseline at `2c92f5a`: all four Go modules above (and `connectors/mediaconv`)
test green.

## Scope

**In scope**:
- `connectors/shared/functions/` (create: `client.go`, `provider.go`,
  `mediaconvert.go`, `functions_test.go`); `connectors/shared/go.mod`, `go.sum`
- `providers/mediaconv/` (create: `go.mod`, `go.sum`, `cmd/wuphf-mediaconv/main.go`,
  `internal/config/config.go`, `internal/config/config_test.go`,
  `internal/mediaconv/` (moved files), `internal/convert/handler.go`,
  `internal/convert/handler_test.go`, `config.example.toml`, `README.md`)
- `connectors/mediaconv/` (moved out, then deleted)
- `connectors/whatsapp/internal/whatsapp/lottie.go`, `lottie_test.go` (moved out)
- Telegram: `internal/telegram/sender.go`, `sender_test.go`, `connector.go`,
  `internal/config/config.go`, `config_test.go`, `config.example.toml`,
  `README.md`, `go.mod`, `go.sum`
- WhatsApp: `internal/whatsapp/sender.go`, `sender_test.go`, `connector.go`,
  `internal/config/config.go`, `config_test.go`, `config.example.toml`,
  `README.md`, `go.mod`, `go.sum`
- `flake.nix` (comment only), `config/example.toml` (comment only),
  `README.md`, `docs/architecture.md`

**Out of scope** (do NOT touch):
- `connectors/instagram` — it has no sticker conversion.
- `connectors/whatsapp-rust` — it has no media support at all yet; a Rust
  caller helper belongs with the plan that adds media to it.
- `connectors/whatsapp/internal/whatsapp/stickermeta.go` — `webpmux`
  pack-metadata embedding is WhatsApp-specific formatting, not conversion; it
  stays in the connector.
- Any Rust crate, proto file or generated binding — plan 032 owns them.
- The conversion algorithms: move them verbatim (package name and exported
  identifiers may change; logic, constants and tool arguments may not).

## Git workflow

- Branch: `advisor/033-mediaconv-provider`.
- Commits:
  1. `feat(shared): add remote function client and provider SDK`
  2. `feat(telegram): convert stickers through media.convert`
  3. `feat(whatsapp): convert stickers through media.convert`
  4. `feat: add mediaconv provider serving media.convert@1`
  5. `refactor: remove the local mediaconv module`
- Use `git mv` for moved files so history follows them.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 0: Baseline

Run the four Go test commands (plus `cd connectors/mediaconv && go test ./...`)
and `cargo test --workspace 2>&1 | grep "^test result" | awk '{s+=$4} END {print s}'`.
Record the results.

### Step 1: The shared function SDK

Create package `functions` in `connectors/shared/functions/`
(import path `github.com/akshettrj/wuphf/connectors/shared/functions`).
Every file starts with a doc comment in the style of `coreclient`.

`client.go` — calling functions:

```go
// Client calls remote functions through the core's FunctionTransport.
type Client struct { /* *grpc.ClientConn, wuphfpb.FunctionTransportClient, token, caller */ }

// Dial connects to the core. token may be empty (core without an invoke
// token); caller labels this process in the core's logs (a connector id).
func Dial(address, token, caller string) (*Client, error)
func (c *Client) Close() error

// Invoke calls name@major. args is marshalled to JSON; the JSON result is
// decoded into result unless result is nil. It returns the result media ids.
// The call's deadline is ctx's deadline.
func (c *Client) Invoke(ctx context.Context, name string, major uint32, args any, mediaIDs []string, result any) ([]string, error)
```
Attach `authorization: Bearer <token>` with
`metadata.AppendToOutgoingContext` when `token != ""`. Use
`grpc.NewClient(address, grpc.WithTransportCredentials(insecure.NewCredentials()))`
exactly as `coreclient.DialMedia` does. Return errors wrapped with
`fmt.Errorf("invoke %s@%d: %w", …)` so `status.Code(errors.Unwrap…)` still
works (`status.FromError` handles wrapped errors in grpc ≥ 1.55).

`provider.go` — serving functions:

```go
type Call struct {
    ID       string
    Caller   string
    Args     json.RawMessage
    MediaIDs []string
}
type Result struct {
    Body     any      // marshalled to JSON
    MediaIDs []string
}
type Handler func(ctx context.Context, call Call) (Result, error)

// Error lets a handler choose the error code the caller sees.
type Error struct {
    Code    wuphfpb.CallErrorCode
    Message string
}
func (e *Error) Error() string
func InvalidArgument(format string, args ...any) error
func Unsupported(format string, args ...any) error

type Function struct {
    Name            string
    Major           uint32
    RequestSchema   string // JSON Schema document
    ResponseSchema  string
    Idempotent      bool
    MaxConcurrency  uint32
    DefaultDeadline time.Duration // 0 = let the core decide
    Handle          Handler
}

type ProviderOptions struct {
    Address        string
    ProviderID     string
    Token          string
    Functions      []Function
    ReconnectDelay time.Duration // 0 = 2s
}

// Serve registers the functions and serves calls until ctx ends, reconnecting
// after transient failures. It returns at once on a rejection that retrying
// cannot fix: Unauthenticated, PermissionDenied or InvalidArgument.
func Serve(ctx context.Context, opts ProviderOptions, logger zerolog.Logger) error
```

`Serve` behaviour:
- Open `OpenProviderSession` with the bearer token in outgoing metadata; send
  the `ProviderHello` (one `FunctionDeclaration` per `Function`;
  `DefaultDeadlineMs` set only when `DefaultDeadline > 0`).
- One goroutine receives frames. A `ProviderCall` starts a goroutine running
  the matching handler with a context that has the call's
  `deadline_unix_ms` as its deadline and is cancelled when a `CancelCall` for
  that id arrives, or when the session ends. Track cancel funcs in a
  mutex-guarded map keyed by call id.
- Results go to the core through **one** sender goroutine fed by a channel
  (`grpc` client streams are not safe for concurrent `Send`). A handler
  returning `*Error` → `CallError{Code, Message}`; any other error →
  `CALL_ERROR_CODE_FAILED` with `err.Error()`; a panic is recovered and
  reported as `FAILED`. A result for a cancelled call is still sent (the core
  ignores it).
- A call for a function this provider did not declare → `UNSUPPORTED` result.
- When the stream ends: cancel every running handler; if `ctx` is done return
  `ctx.Err()`; if the status code is `Unauthenticated`, `PermissionDenied` or
  `InvalidArgument`, return the error; otherwise log a warning (zerolog
  `Warn().Err(err)`), wait `ReconnectDelay` (abort on `ctx.Done()`), and
  reconnect. `AlreadyExists` is retried (another provider may release the
  name).

`mediaconvert.go` — the `media.convert@1` contract, shared by callers and the
provider so both sides use the same types:

```go
const (
    MediaConvertName  = "media.convert"
    MediaConvertMajor = 1
)

// MediaConvertArgs are the args of media.convert@1. The source media is the
// call's single media id.
type MediaConvertArgs struct {
    SourceMimeType string `json:"source_mime_type,omitempty"`
    SourceFileName string `json:"source_file_name,omitempty"`
    TargetMimeType string `json:"target_mime_type"`
}

// MediaConvertResult is the result of media.convert@1. The converted media is
// the result's single media id.
type MediaConvertResult struct {
    MimeType string `json:"mime_type"`
    FileName string `json:"file_name"`
}

// MediaDownloader fetches stored media by id (coreclient.MediaClient satisfies it).
type MediaDownloader interface {
    Download(ctx context.Context, mediaID string) ([]byte, error)
}

// MediaConverter converts stored media through media.convert@1 and returns the
// converted bytes.
type MediaConverter struct {
    Functions *Client
    Media     MediaDownloader
    Timeout   time.Duration // bounds the whole call, including waiting for a provider
}

func (m MediaConverter) Convert(ctx context.Context, mediaID, sourceMime, sourceName, targetMime string) ([]byte, error)
```
`Convert` applies `Timeout` with `context.WithTimeout`, invokes, requires
exactly one result media id (else an error), and downloads it.

`functions_test.go` — tests against a fake core started on
`google.golang.org/grpc/test/bufconn` that implements both
`wuphfpb.FunctionTransportServer` and `wuphfpb.ProviderTransportServer`
(embed the generated `Unimplemented…Server` structs). Dial bufconn with
`grpc.NewClient("passthrough:///bufnet", grpc.WithContextDialer(…), insecure)`;
add an unexported `dialer` hook to `Dial`/`Serve` options **only if** needed
for this (keep it unexported). Cases are listed in the Test plan.

**Verify**: `cd connectors/shared && go build ./... && go vet ./... && go test ./...`
→ exit 0.

Commit 1.

### Step 2: Telegram calls `media.convert`

- `internal/config/config.go`: add to `File`

  ```go
  // FunctionsTokenEnv names the environment variable holding the core's
  // invoke token, when the core requires one.
  FunctionsTokenEnv string `toml:"functions_token_env"`
  // MediaConvertTimeoutMs bounds a sticker conversion, including waiting for
  // the converter; deliveries on this connector wait behind it. 0 = 20000.
  MediaConvertTimeoutMs uint32 `toml:"media_convert_timeout_ms"`
  ```
  and `func (c File) MediaConvertTimeout() time.Duration` applying the
  default. Add a `config_test.go` case for the default and an explicit value.
- `internal/telegram/sender.go`:
  - add a consumer-side interface next to `MediaBroker`:

    ```go
    // StickerConverter converts stored media to another format (in production,
    // functions.MediaConverter calling media.convert@1 on the core).
    type StickerConverter interface {
        Convert(ctx context.Context, mediaID, sourceMime, sourceName, targetMime string) ([]byte, error)
    }
    ```
  - `Sender` gains `converter StickerConverter`; `NewSender(api, peers, media,
    converter, rendering)`.
  - `animatedStickerWebM` keeps the `video/webm` pass-through; otherwise
    `return s.converter.Convert(ctx, attachment.GetMediaId(), attachment.GetMimeType(), attachment.GetFileName(), "video/webm")`.
  - Update `sendSticker`'s doc comment: the transcode runs on the core's
    `media.convert` provider; when it is unavailable or fails, the sticker
    degrades to a static sticker.
  - Remove the `mediaconv` import.
- `internal/telegram/connector.go`: after `DialMedia`, dial
  `functions.Dial(c.cfg.CoreAddress, os.Getenv(c.cfg.FunctionsTokenEnv), c.cfg.ConnectorID)`
  (only call `os.Getenv` when `FunctionsTokenEnv != ""`), `defer` its
  `Close`, and pass `functions.MediaConverter{Functions: fns, Media: media, Timeout: c.cfg.MediaConvertTimeout()}`
  to `NewSender`.
- `sender_test.go`: update `NewSender` call sites; add tests with a fake
  converter (records its arguments, returns fixed bytes or an error):
  - `TestAnimatedStickerWebMPassesWebMThrough` — `video/webm` source; the
    converter is not called; bytes unchanged.
  - `TestAnimatedStickerWebMConvertsWebP` — `image/webp` source; the converter
    is called once with the attachment's media id, mime, name and
    `video/webm`; its bytes are returned.
  - `TestAnimatedStickerWebMReturnsConverterError`.
- `go.mod`: remove the `mediaconv` require and `replace`; `go mod tidy`.
- `config.example.toml`: document both new keys (commented, with defaults),
  and that animated stickers need the `mediaconv` provider (else they arrive
  as static stickers).
- `README.md`: replace any statement that the connector needs ffmpeg/
  ImageMagick with the provider requirement and the sequential-delivery
  warning (see Maintenance notes, first bullet).

**Verify**: `cd connectors/telegram && go build ./... && go vet ./... && go test ./...`
→ exit 0; `grep -rn "connectors/mediaconv" connectors/telegram --include='*.go' --include=go.mod`
→ no matches.

Commit 2.

### Step 3: WhatsApp calls `media.convert`

- `internal/config/config.go`: the same two fields, method and tests as
  Telegram.
- `internal/whatsapp/sender.go`:
  - the same `StickerConverter` interface; `Sender` gains `converter`;
    `NewSender(client, media, converter, ownJID, stickerMeta, rendering)`.
  - replace the free function `animatedStickerWebP` with a method:

    ```go
    func (s Sender) animatedStickerWebP(ctx context.Context, attachment *wuphfpb.Attachment, mimeType string) ([]byte, error) {
        return s.converter.Convert(ctx, attachment.GetMediaId(), mimeType, attachment.GetFileName(), "image/webp")
    }
    ```
    and call it from `stickerMessage` (the `data` argument is no longer
    needed by it). The document fallback on error is unchanged.
  - Pass `mimeType` (the value `stickerMessage` receives), not
    `attachment.GetMimeType()`, so the provider sees the same type the old
    code dispatched on.
  - Keep `stickerSourceExt` and `stickerFileName` (still used by the
    fallback's file name).
  - Remove the `mediaconv` import. Leave `lottie.go` where it is: Step 4
    moves it into the provider. Until then its `tgsToAnimatedWebP` is unused,
    which still compiles and passes `go vet`.
  - Update `stickerMessage`'s doc comment like Telegram's.
- `internal/whatsapp/connector.go`: dial and pass the converter as in
  Telegram.
- `sender_test.go`: update every `NewSender` call site (pass a
  `stubConverter{}`); add:
  - `TestAnimatedStickerIsConvertedToWebP` — a `video/webm` animated sticker;
    the converter is called with `image/webp` as the target; the uploaded
    message is a `StickerMessage` with mimetype `image/webp`.
  - `TestTGSStickerIsConvertedToWebP` — `application/x-tgsticker`; the
    converter receives that source mime.
  - `TestStickerConversionFailureFallsBackToDocument` — converter error →
    `DocumentMessage`.
  - `TestStaticWebPStickerSkipsConversion` — `image/webp` source; converter
    not called.
- `go.mod`: remove the `mediaconv` require/replace; `go mod tidy`.
  `tgsconverter` stays until Step 4 (`lottie.go` still imports it).
- `config.example.toml`, `README.md`: as Telegram.

**Verify**: `cd connectors/whatsapp && go build ./... && go vet ./... && go test ./...`
→ exit 0; `grep -rn "connectors/mediaconv" connectors/whatsapp --include='*.go' --include=go.mod`
→ no matches.

Commit 3.

### Step 4: Create the provider and move the conversions into it

Create module `providers/mediaconv/go.mod`:

```
module github.com/akshettrj/wuphf/providers/mediaconv

go 1.25.0

replace github.com/akshettrj/wuphf/proto/go => ../../proto/go

replace github.com/akshettrj/wuphf/connectors/shared => ../../connectors/shared
```
(requirements are filled by `go mod tidy` below.)

Move the conversion code:

```sh
mkdir -p providers/mediaconv/internal/mediaconv
git mv connectors/mediaconv/mediaconv.go providers/mediaconv/internal/mediaconv/mediaconv.go
git mv connectors/mediaconv/mediaconv_test.go providers/mediaconv/internal/mediaconv/mediaconv_test.go
git mv connectors/whatsapp/internal/whatsapp/lottie.go providers/mediaconv/internal/mediaconv/lottie.go
git mv connectors/whatsapp/internal/whatsapp/lottie_test.go providers/mediaconv/internal/mediaconv/lottie_test.go
```
Then, in the two moved Lottie files only:
- `package whatsappconnector` → `package mediaconv`.
- Rename `tgsToAnimatedWebP` → `TGSToAnimatedWebP` (definition and test).
- Delete the `isAnimatedWebP` function from `lottie_test.go` (the identical
  one in `mediaconv_test.go` remains). If the two bodies differ, STOP.
- In `lottie.go`'s doc comment, replace the paragraph about living "here,
  with its sole caller, so the shared mediaconv module and the Telegram
  connector stay cgo-free" with one sentence: the renderer links C/C++ (cgo),
  which is confined to this provider.

`internal/convert/handler.go` — the `media.convert@1` handler:

```go
// MediaBroker is the core media store (coreclient.MediaClient satisfies it).
type MediaBroker interface {
    Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error)
    Download(context.Context, string) ([]byte, error)
}

// Function returns the media.convert@1 declaration and handler.
func Function(media MediaBroker, maxConcurrency uint32, defaultDeadline time.Duration) functions.Function
```
- `Name`/`Major` from `functions.MediaConvertName`/`MediaConvertMajor`;
  `Idempotent: true` (a pure conversion); `RequestSchema`/`ResponseSchema`
  are JSON Schema documents (as Go string constants) describing
  `MediaConvertArgs`/`MediaConvertResult`, with `target_mime_type` required.
- Handler:
  1. Decode args into `functions.MediaConvertArgs`; decode error or empty
     `target_mime_type` → `functions.InvalidArgument`.
  2. `len(call.MediaIDs) != 1` → `InvalidArgument("media.convert takes exactly one media id")`.
  3. Download the source.
  4. `ext := sourceExt(args.SourceMimeType, args.SourceFileName)` — a copy of
     WhatsApp's `stickerSourceExt` logic over strings: `"video/webm"`→`.webm`,
     `"application/x-tgsticker"`→`.tgs`, `"image/webp"`→`.webp`, else
     `path.Ext(name)`, else `.bin`.
  5. Pick the conversion:
     - target `video/webm` and ext `.webp` → `mediaconv.AnimatedWebPToWebM`
       (file name `sticker.webm`)
     - target `image/webp` and ext `.tgs` → `mediaconv.TGSToAnimatedWebP`
       (file name `sticker.webp`)
     - target `image/webp`, any other ext → `mediaconv.ToAnimatedWebP(ctx, data, ext)`
       (file name `sticker.webp`)
     - anything else → `functions.Unsupported("cannot convert %s to %s", …)`
  6. A `*mediaconv.MissingToolError` → plain error (reported as `FAILED`) with
     its message; other conversion errors likewise.
  7. Upload the output with `MediaMetadata{Kind: wuphfpb.Attachment_KIND_STICKER,
     FileName, MimeType: target, SizeBytes: len, Animated: true}`; return
     `Result{Body: functions.MediaConvertResult{MimeType, FileName}, MediaIDs: [uploaded.MediaId]}`.

`internal/config/config.go` — TOML, modeled on the connectors' config:

```go
type File struct {
    CoreAddress string `toml:"core_address"`
    ProviderID  string `toml:"provider_id"`
    // TokenEnv names the environment variable holding this provider's token;
    // it must match the core's [[functions.providers]] entry.
    TokenEnv string `toml:"token_env"`
    LogLevel string `toml:"log_level"`
    // MaxConcurrency bounds simultaneous conversions; 0 = runtime.NumCPU().
    MaxConcurrency uint32 `toml:"max_concurrency"`
    // DefaultDeadlineMs is the deadline suggested to the core; 0 = core default.
    DefaultDeadlineMs uint32 `toml:"default_deadline_ms"`
    // ReconnectDelayMs is the pause before reconnecting to the core; 0 = 2000.
    ReconnectDelayMs uint32 `toml:"reconnect_delay_ms"`
}
func Load(path string) (File, error)
func (c File) Validate() error // core_address, provider_id and token_env are required
```
with a `config_test.go` covering a valid file and each missing required key.

`cmd/wuphf-mediaconv/main.go`: flag `-config` (default
`config.example.toml`); `logging.New(cfg.LogLevel)`; read the token with
`os.Getenv(cfg.TokenEnv)` and exit with an error naming the variable if it is
empty; `coreclient.DialMedia(cfg.CoreAddress)`; `signal.NotifyContext` on
`os.Interrupt`; `functions.Serve(ctx, ProviderOptions{…, Functions:
[]functions.Function{convert.Function(media, …)}}, logger)`. Mirror
`connectors/telegram/cmd/wuphf-telegram/main.go`'s `exit` helper style.

`config.example.toml`:

```toml
core_address = "127.0.0.1:50051"
# Must match an [[functions.providers]] id in the core configuration, whose
# `functions` list must allow "media.convert".
provider_id = "mediaconv"
# The token is read from this environment variable; the core reads the same
# token from the variable named by its own token_env.
token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
# max_concurrency = 0        # 0 = number of CPUs
# default_deadline_ms = 0    # 0 = the core's default
# reconnect_delay_ms = 2000
```

`README.md`: what the provider serves (the `media.convert@1` contract and the
three conversions), the tools it needs on PATH (`ffmpeg`, ImageMagick) and at
build time (cgo + a C++ toolchain for rlottie), how to configure it on both
sides, and how to check it registered (`wuphf functions list`).

Run `cd providers/mediaconv && go mod tidy`.

**Verify**: `cd providers/mediaconv && go build ./... && go vet ./... && go test ./...`
→ exit 0; `go test ./internal/mediaconv/ -run 'TestTGSToAnimatedWebP|TestToAnimatedWebPFromWebM|TestAnimatedWebPToWebM' -v`
→ the tests run (or skip only for a missing tool) and pass.

Now that `lottie.go` has left WhatsApp, drop its renderer dependency:
`cd connectors/whatsapp && go mod tidy` (removes
`github.com/watgbridge/tgsconverter` and the indirect `go_rlottie`).

**Verify**: `cd connectors/whatsapp && go build ./... && go vet ./... && go test ./...`
→ exit 0; `grep -rn "connectors/mediaconv\|tgsconverter\|rlottie" connectors/whatsapp --include='*.go' --include=go.mod --include=go.sum`
→ no matches; `cd connectors/telegram && go build ./...` → exit 0 (neither
connector imports `connectors/mediaconv` any more, so moving its files out
breaks nothing).

Commit 4.

### Step 5: Remove the local module and update docs

- `git rm -r connectors/mediaconv` (only `go.mod` should be left in it; if
  anything else is, STOP).
- `flake.nix`: keep the `ffmpeg`/`imagemagick` packages; change their comment
  to say they are used by the `providers/mediaconv` sticker converter.
- `config/example.toml`: in the commented `[functions]` section plan 032
  added, add a line pointing at `providers/mediaconv/config.example.toml`.
- `README.md` Layout block: add a `providers/mediaconv/` line ("Sticker
  converter serving media.convert@1"); remove any `connectors/mediaconv` line.
- `docs/architecture.md`: in the "Remote functions" paragraph, name
  `media.convert@1` as the first function and `providers/mediaconv` as its
  provider.

**Verify**: `grep -rn "connectors/mediaconv" --include='*.go' --include='*.mod' --include='*.md' --include='*.nix' --include='*.toml' . | grep -v "^./plans/"`
→ no matches; `test -d connectors/mediaconv` → exit 1.

Commit 5.

### Step 6: End-to-end smoke test

In the nix shell, with throwaway tokens:

```sh
export WUPHF_PROVIDER_MEDIACONV_TOKEN=smoke-provider
SCRATCH=$(mktemp -d)
cat > "$SCRATCH/core.toml" <<'EOF'
listen = "127.0.0.1:50999"
database_url = "sqlite::memory:"
[functions]
[[functions.providers]]
id = "mediaconv"
token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
functions = ["media.convert"]
EOF
sed 's/50051/50999/' providers/mediaconv/config.example.toml > "$SCRATCH/mediaconv.toml"
cargo run -q -p wuphf-cli -- serve --config "$SCRATCH/core.toml" & CORE=$!
(cd providers/mediaconv && go run ./cmd/wuphf-mediaconv -config "$SCRATCH/mediaconv.toml") & PROVIDER=$!
# wait until the core logs "provider session registered", then:
cargo run -q -p wuphf-cli -- functions list --address http://127.0.0.1:50999
kill $PROVIDER $CORE
```

**Verify**: the `functions list` output has a line starting
`media.convert@1  provider=mediaconv`. If `sqlite::memory:` is rejected by
`SqliteStorage::connect`, use `sqlite://$SCRATCH/core.sqlite?mode=rwc`
instead. Wait for the log line by watching the process output, not with a
fixed sleep.

### Step 7: Full gate

**Verify**: all four Go module commands → exit 0; `cargo test --workspace` →
the Step 0 count; `git diff --stat 2c92f5a..HEAD -- . ':!plans'` lists only
in-scope paths (plus plan 032's, if this branch sits on top of it).

## Test plan

- `connectors/shared/functions/functions_test.go` (fake core on bufconn):
  1. `TestInvokeSendsArgsTokenAndCaller` — the fake sees the JSON args, media
     ids, `FunctionRef`, caller and `authorization: Bearer <token>`; the
     result decodes into the given struct and media ids are returned.
  2. `TestInvokeOmitsAuthorizationWithoutToken`.
  3. `TestInvokeReturnsStatusCode` — fake returns `NOT_FOUND`;
     `status.Code(err) == codes.NotFound`.
  4. `TestServeRegistersAndAnswersCalls` — the hello carries every
     declaration field; a call is answered with the handler's JSON body and
     media ids.
  5. `TestServeMapsHandlerErrors` — `InvalidArgument`, `Unsupported`, plain
     error → `FAILED`, panic → `FAILED`.
  6. `TestServeCancelsHandlerOnCancelCall` — the handler's ctx is done after
     the fake sends `CancelCall`.
  7. `TestServeAppliesCallDeadline` — the handler's ctx deadline equals
     `deadline_unix_ms`.
  8. `TestServeStopsOnPermanentRejection` — fake rejects the session with
     `PermissionDenied`; `Serve` returns that error without retrying.
  9. `TestServeReconnectsAfterTransientFailure` — fake ends the first stream
     with `Unavailable`; a second hello arrives (use a 10 ms `ReconnectDelay`).
  10. `TestMediaConverterDownloadsResult` and
      `TestMediaConverterRejectsWrongMediaCount`.
- `providers/mediaconv/internal/convert/handler_test.go` (fake `MediaBroker`
  in memory): invalid args; wrong media count; unsupported pair →
  `*functions.Error` with `UNSUPPORTED`; WebM→WebP end to end (skip without
  ffmpeg, using the moved `requireTool` helper pattern); the upload metadata
  (kind sticker, animated, mime, file name); `sourceExt` table test.
- Moved tests keep passing unchanged: `TestToAnimatedWebPFromWebM`,
  `TestAnimatedWebPToWebM`, `TestMissingToolErrorReported`,
  `TestTGSToAnimatedWebP`.
- Connector sender tests listed in Steps 2–3.

## Done criteria

- [ ] `go build`, `go vet` and `go test` exit 0 in `connectors/shared`,
      `providers/mediaconv`, `connectors/telegram`, `connectors/whatsapp`
- [ ] `test -d connectors/mediaconv` exits 1
- [ ] `grep -rn "connectors/mediaconv\|tgsconverter\|rlottie" connectors/telegram connectors/whatsapp --include='*.go' --include=go.mod --include=go.sum` → no matches
- [ ] the new tests named in the Test plan exist and pass
- [ ] Step 6 printed `media.convert@1  provider=mediaconv`
- [ ] `cargo test --workspace` count unchanged from Step 0
- [ ] `plans/README.md` status row updated

## STOP conditions

- Plan 032 is not done (drift-check commands fail), or the generated Go
  bindings lack a type this plan names.
- The two `isAnimatedWebP` bodies differ.
- A moved conversion function needs a logic, constant or tool-argument change
  to compile or pass its tests.
- `go mod tidy` in WhatsApp keeps `tgsconverter` or `go_rlottie` (something
  else still imports them — report what).
- The sender tests show a behavior change beyond "conversion goes through the
  converter" (for example, the static-sticker or document fallback no longer
  triggers on a conversion error).
- The smoke test cannot register the provider after one fix attempt (report
  the core and provider logs).

## Maintenance notes

- **Operational trade-off of "wait for a provider"**: delivery on a
  connector is sequential. If the core's config allows a `media.convert`
  provider but none is running, every animated sticker waits up to
  `media_convert_timeout_ms` (default 20 s) before falling back, and that
  connector's later messages wait behind it. If you do not run the provider,
  remove its `[[functions.providers]]` entry from the core config: calls then
  fail at once with `NOT_FOUND` and fall back immediately. State this in
  both connector READMEs.
- The `media.convert@1` contract carries no size or duration limits; the
  provider applies the constants the old code used (Telegram's 512 px, 3 s,
  600 kbit/s WebM; WhatsApp's 1 MiB WebP). Adding optional
  `max_edge_px`/`max_duration_ms`/`max_bytes` args is a compatible change
  (same major) when a connector needs different limits.
- Rewriting the provider in another language only requires speaking the two
  proto services and the `media.convert@1` JSON contract (the JSON Schemas in
  `internal/convert/handler.go` are its reference); connectors need no change.
- A Rust caller helper (`wuphf-connector`) should come with the plan that
  gives `connectors/whatsapp-rust` media support; it has none today.
- Reviewers: check that each connector's fallback path is reached when the
  converter errors (tests in Steps 2–3) and that no conversion constant
  changed in the moved files (`git diff -M --stat` should show them as
  renames with small similarity loss only in `lottie.go`).
