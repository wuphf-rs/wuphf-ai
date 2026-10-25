# Plan 041: A settings panel in the Telegram connector

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- connectors/telegram connectors/shared`
> Plan 040 is expected to have split the Telegram config into file-only and
> runtime parts and to make senders read a `settings.Live` value. What must
> still hold: `Normalizer.normalize` drops `message.Out` messages before
> building an event (`connectors/telegram/internal/telegram/updates.go:136-138`);
> `connector.go` builds one gotd client with one `tg.UpdateDispatcher` and
> already answers `wuphf:label` callback queries
> (`connector.go:60-69`). Otherwise STOP.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED — intercepts messages before mirroring (a wrong match would
  swallow chat content) and adds a second Telegram client to the process
- **Depends on**: 039 (settings service, owner authorization), 040 (shared Go
  settings package); botconf plans 004 (screen engine), 005 (`Render`/`Act`),
  006 (Go client) and 007 (gotd renderers, bot and inline) landed and tagged
- **Category**: direction
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

The owner wants to change wuphf's settings from Telegram: a `/wuphf` panel of
buttons like the assistant bot's `/config`, backed by the core's settings
store. botconf's screen engine (plan 004) builds every screen in the core;
the Go SDK (plans 006–007) renders screens to Telegram. This plan wires that
into the Telegram connector, which is a thin front end: it recognizes the
command, says who is acting, renders what the core returns, and forwards
presses and typed answers. The core authorizes every request (039).

A Telegram **user** session cannot attach callback buttons to its messages;
only bots can. For user sessions the panel uses the inline-bot trick: the
connector also runs a small helper bot, the user session sends the panel as
that bot's inline result, and the presses reach the helper bot.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | The command is `/wuphf` by default, configurable per connector (`panel.command`, file-only). |
| 2 | The connector checks for the command **before** the `message.Out` drop, because a user session's owner types from the session's own account. A matching message is **never mirrored, whoever sends it**. |
| 3 | The connector sends the actor's Telegram user id with every `Render`/`Act`; the core decides (039). Unauthorized button presses get a "not allowed" callback toast; an unauthorized `/wuphf` gets no reply. |
| 4 | Bot sessions send the panel with an inline keyboard directly. User sessions use a **helper bot run as a second gotd client in the same process** (token from `WUPHF_TELEGRAM_PANEL_BOT_TOKEN`, inline mode enabled in @BotFather) and the inline-bot trick: `messages.getInlineBotResults` + `messages.sendInlineBotResult`, presses arrive as `updateInlineBotCallbackQuery`, edits via `messages.editInlineBotMessage`. |
| 5 | The command message is deleted after the panel opens, where the account has the right to. |
| 6 | Text subcommands work: `/wuphf set <key> <value>` and the rest are passed as text to `Act`. |
| 7 | Prompts for typed values (from the screen engine) are answered by the owner's next message replying to the panel or prompt; that message goes to `Act`, is not mirrored, and is deleted. |

### Decisions made by this plan (confirmed by the owner 2026-10-25)

| # | Decision | Why |
|---|---|---|
| F1 | If the inline route fails for a user session (helper bot unset or down, inline bots forbidden in that chat, timeout), the panel is posted as **plain text** listing the screen and the matching text subcommands. | The panel must still be usable; text subcommands cover every action. |
| F2 | Inline queries carry a **single-use random nonce** (16 bytes, base64url) that the connector maps to the screen it asked the core for; the helper bot answers only queries whose `user_id` is the user session's own account and whose nonce is pending. | Anyone can type `@helperbot …` in any chat; the nonce and the user check stop strangers from fetching or spoofing panels. |
| F3 | Callback data is botconf's own prefix (`cfg:`, from 004/007) plus its token; `wuphf:label` stays separate. | Keeps the existing sticker-label answer untouched. |
| F4 | The helper bot keeps its own session file (`panel.session_file`), next to the main one. | gotd session storage is per client. |

## Current state

- `connectors/telegram/internal/telegram/updates.go:26-46` — `Dispatcher()`
  registers `OnNewMessage`, `OnNewChannelMessage`, edit and delete handlers.
- `updates.go:65-82` — `handle` calls `normalize` and pushes a `MessageCreate`
  frame onto `inputs`.
- `updates.go:127-138` — the drop this plan must run before:

  ```go
  message, ok := class.(*tg.Message)
  if !ok {
      return nil, false, nil
  }
  if message.Out {
      return nil, false, nil
  }
  ```
- `updates.go:182-187` — `senderID(message)` formats `telegram:user:<id>`
  from `FromID`, else the peer. The actor id sent to the core is the bare
  numeric user id (what `owner_ids` holds, 039), not the formatted peer.
- `connector.go:58-73` — one `tg.UpdateDispatcher` from `NewNormalizer(...)`,
  plus `OnBotCallbackQuery` answering `labelCallbackData` with
  `MessagesSetBotCallbackAnswer`; `gotd.NewClient(...)` with
  `session.FileStorage{Path: c.cfg.SessionFile}`; `client.Run(ctx, …)`
  authenticates, primes peers, builds the `Sender` and runs `coreclient`.
- `connector.go:82-86` — bot vs user session is `c.credentials.BotToken != ""`.
- `config/config.go:115-139` — `LoadCredentials` reads `WUPHF_TELEGRAM_*`.
- `reset.go:42` — `client.Self(ctx)` gives the session's own user (its id is
  the actor for `Out` messages).
- Tests: `updates_test.go` builds `tg.Message` values and calls
  `normalizer.normalize` directly; `sticker_label_test.go:15-36` has a
  `recordingInvoker` that records API requests and answers with
  `UpdateShortSentMessage`.
- botconf Go SDK (plans 006–007; **check the tagged API**, STOP if the
  concepts are missing):

  ```go
  // client (006)
  func Dial(address, token string) (*botconf.Client, error)
  func (c *Client) Render(ctx context.Context, page string, actor string) (*Screen, error)
  func (c *Client) Act(ctx context.Context, action Action) (*Outcome, error)
  type Action struct { Token string; Text string; Actor string; Chat string }
  type Outcome struct { Screen *Screen; Toast string; Prompt *Prompt; Closed bool }

  // renderers (007)
  package gotdbot    // Markup(*Screen) tg.ReplyMarkupClass; Text(*Screen) []styling.StyledTextOption
  package gotdinline // Result(id string, *Screen) tg.InputBotInlineResultClass; Edit(…) request
  ```

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Telegram tests | `cd connectors/telegram && go vet ./... && go test -race ./...` | exit 0 |
| Shared tests | `cd connectors/shared && go test -race ./...` | exit 0 |
| Build | `cd connectors/telegram && go build ./...` | exit 0 |

## Scope

**In scope**:
- `connectors/telegram/internal/config` (`[panel]` section, panel bot token)
- `connectors/telegram/internal/telegram/updates.go` (interception hook)
- `connectors/telegram/internal/telegram/panel.go`, `panel_inline.go`,
  `panel_test.go` (create)
- `connectors/telegram/internal/telegram/connector.go` (wiring, helper client)
- `connectors/telegram/go.mod` / `go.sum` (botconf Go module)
- `connectors/telegram/config.example.toml`, `connectors/telegram/README.md`

**Out of scope**:
- Any core change (the core's side is 039).
- Stickers through the helper bot (043).
- Discord or WhatsApp panels.

## Git workflow

- Commit on the current branch. Commits:
  1. `feat(telegram): intercept the settings command before mirroring`
  2. `feat(telegram): render the settings panel for bot sessions`
  3. `feat(telegram): run a helper bot for user-session panels`
  4. `docs(telegram): describe the settings panel`
- Do NOT push.

## Steps

### Step 0: Baseline

`cd connectors/telegram && go test -race ./...` → exit 0; record the count
(`go test ./... -v 2>&1 | grep -c '^--- PASS'`).

### Step 1: Configuration

Add to the file config (file-only, not part of 040's runtime struct):

```go
// Panel is the settings panel opened with a command. Empty Command means
// "wuphf".
Panel PanelOptions `toml:"panel"`

type PanelOptions struct {
    Command     string `toml:"command"`
    // Where the helper bot keeps its session (user sessions only).
    SessionFile string `toml:"session_file"`
}
```

`Validate`: the command is 1–32 characters of `[a-z0-9_]`. `LoadCredentials`
reads `WUPHF_TELEGRAM_PANEL_BOT_TOKEN` into `Credentials.PanelBotToken`; for a
user session with a panel bot token, `panel.session_file` is required.

**Verify**: config tests `TestPanelCommandDefaultsToWuphf`,
`TestPanelCommandMustBeLowercaseWord`,
`TestPanelSessionFileRequiredWithPanelBot`.

### Step 2: Intercept before the drop

Add to `Normalizer` an optional `panel PanelRouter`:

```go
// PanelRouter takes the messages that drive the settings panel: the panel
// command and answers to a pending prompt. It reports whether it took the
// message; a taken message is never mirrored.
type PanelRouter interface {
    Take(ctx context.Context, entities tg.Entities, message *tg.Message) bool
}
```

In `normalize`, right after the `*tg.Message` type assertion and **before**
`if message.Out`, call `n.panel.Take(...)` when `panel != nil`; if it took the
message, return `nil, false, nil`. In `normalizeEdit`, ignore edits of
messages the router took (it remembers their ids for an hour; edits of a
command must not be mirrored either).

`Take` matches, on `message.Message`:
- `/<command>` or `/<command>@<bot username>` as the first word (case
  sensitive, like Telegram commands), with or without arguments;
- or any message from the actor of a pending prompt in that chat that replies
  to the prompt or panel message (decision 7).

Actor: `message.FromID` when it is a `PeerUser`; for `Out` messages without
`FromID` (private chats), the session's own user id (cached from
`client.Self` at startup). No user actor (channel posts) → not taken.

**Verify**: normalizer tests in the test plan (1–5) pass.

Commit 1.

### Step 3: Bot sessions

Create `panel.go` with a `Panel` type implementing `PanelRouter`:

- On a command: delete the command message (best effort, log at debug on
  failure); `Render(home, actor)` (or `Act(Text: args)` when arguments
  follow); on `PermissionDenied` do nothing more; otherwise send the screen
  to the chat with `gotdbot.Text` and `gotdbot.Markup`, replying in the same
  forum topic when the command came from one.
- Register on the dispatcher `OnBotCallbackQuery` for data with the botconf
  prefix (F3): `Act(Token, Actor)`; answer the callback with `Outcome.Toast`
  (or `"not allowed"` on `PermissionDenied`, `Alert: false`); edit the panel
  message to the new screen with `messages.editMessage`; on `Closed`, delete
  it. Keep the existing `labelCallbackData` branch as is.
- On `Outcome.Prompt`: send the prompt with `ForceReply` (bots can), remember
  `(chat, actor) → prompt message id` until answered, cancelled (`/cancel`)
  or 10 minutes pass; the answer goes to `Act(Text)`, and the prompt and the
  answer are deleted.

**Verify**: panel tests 6–10 pass.

Commit 2.

### Step 4: User sessions through the helper bot

Create `panel_inline.go`:

- In `connector.go`, when the session is a user session and
  `PanelBotToken` is set, build a second `gotd.NewClient` with
  `session.FileStorage{Path: cfg.Panel.SessionFile}` and its own
  `tg.UpdateDispatcher`, authenticate it with `Auth().Bot(token)` (reusing
  the stored-session check in `authenticate`), and run it alongside the main
  client (`errgroup`; either ending ends the connector, so the reconnect
  logic stays in one place). Resolve the helper's username from
  `helper.Self` and the user session's peer for it with
  `contacts.resolveUsername`.
- Opening a panel: `Render` as in Step 3, store the screen under a fresh
  nonce (F2), then on the **user** client
  `MessagesGetInlineBotResults(bot, peer, query: nonce)` and
  `MessagesSendInlineBotResult(peer, queryID, resultID)` (reply to the topic
  root when needed).
- Helper dispatcher `OnBotInlineQuery`: answer only when `UserID` is the
  user session's id and the nonce is pending; consume the nonce;
  `MessagesSetInlineBotResults` with one `gotdinline.Result`, `CacheTime: 0`,
  `Private: true`. Otherwise answer with no results.
- Helper dispatcher `OnInlineBotCallbackQuery`: `Act(Token, Actor: UserID)`,
  answer the callback (toast or "not allowed"), and edit with
  `MessagesEditInlineBotMessage(update.MsgID, …)`.
- Prompts: the user session cannot force a reply; the screen engine's prompt
  text tells the owner to reply to the panel. The owner's reply is taken by
  `Take` (it is `Out`), sent to `Act(Text)` and deleted.
- Fallback (F1): if any of the above fails or takes over 5 seconds, send
  `gotdbot.Text(screen)` plus the text subcommands as a plain message.

**Verify**: panel tests 11–14 pass, with a fake helper wired to the same
recording invoker pattern.

Commit 3.

### Step 5: Document

`config.example.toml`: a commented `[panel]` table (`command`,
`session_file`) and a comment block on `WUPHF_TELEGRAM_PANEL_BOT_TOKEN`:
create a bot with @BotFather, enable inline mode (`/setinline`), and that the
user session's account id must be in the core's `owner_ids` for this
connector. `README.md`: a "Settings panel" section — commands, bot vs user
session, the "via @bot" line Telegram shows under inline messages, the text
fallback.

Commit 4.

### Step 6: Full gate

`cd connectors/telegram && gofmt -l . && go vet ./... && go test -race ./...`
→ no files listed, exit 0.

## Test plan

In `updates_test.go` / `panel_test.go`:

1. `TestCommandIsTakenNotMirrored` — `/wuphf` from a user; `normalize`
   returns not-ok, the router saw it.
2. `TestOwnCommandIsTakenBeforeOutDrop` — `Out: true` `/wuphf`; the router
   saw it with the session's own id as actor.
3. `TestOtherOutMessagesStillDropped` — `Out: true` "hello" is dropped and
   not taken.
4. `TestCommandWithBotSuffixIsTaken` (`/wuphf@mybot set x 1`) and
   `TestLookalikeTextIsMirrored` (`/wuphfx`, `please /wuphf`).
5. `TestEditOfTakenMessageIsNotMirrored`.
6. `TestBotPanelSendsInlineKeyboard` — recording invoker sees a send with
   `ReplyInlineMarkup`.
7. `TestPressRoutesToActAndEditsPanel`.
8. `TestUnauthorizedPressGetsNotAllowedToast` and
   `TestUnauthorizedCommandGetsNoReply`.
9. `TestTextSubcommandGoesToAct`.
10. `TestPromptAnswerGoesToActAndIsDeleted`.
11. `TestUserSessionSendsPanelThroughInlineResult` — get + send inline result
    requests, in order, with the pending nonce.
12. `TestHelperIgnoresQueriesFromOtherUsersOrUnknownNonces` (F2).
13. `TestInlinePressEditsInlineMessage`.
14. `TestInlineFailureFallsBackToText` (F1).

Plus the three config tests from Step 1.

## Done criteria

- [ ] All tests above exist and pass with `-race`; `gofmt -l .` prints nothing
- [ ] No message matching the command reaches `inputs` in any test
- [ ] The `wuphf:label` answer still works (`sticker_label_test.go` passes)
- [ ] `plans/README.md` row updated; F1–F4 recorded in the dependency notes

## STOP conditions

- botconf's tagged Go SDK has no inline renderer or no way to pass an actor
  and a short action token.
- gotd v0.144 lacks `MessagesGetInlineBotResults`,
  `MessagesSendInlineBotResult`, `MessagesSetInlineBotResults`,
  `MessagesEditInlineBotMessage` or the `OnBotInlineQuery` /
  `OnInlineBotCallbackQuery` dispatcher hooks.
- Intercepting before the `Out` drop would change what is mirrored for any
  message other than the command and prompt answers.
- Running the helper client needs changes to `coreclient`.

## Maintenance notes

- Telegram shows "via @<helper bot>" under inline-sent messages; `hide_via`
  is honoured only for some official bots.
- Groups can forbid inline bots for members; the text fallback (F1) covers
  it.
- 043 reuses the helper client for stickers; keep the client and its
  dispatcher reachable from the sender.
