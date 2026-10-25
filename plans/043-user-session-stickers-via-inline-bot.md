# Plan 043: Label user-session stickers through the helper bot

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 643829b..HEAD -- connectors/telegram`
> Plans 040 and 041 are expected to have changed this connector (live
> settings in the sender; a helper bot client and `panel_inline.go`). What
> must still hold: `Sender.sendSticker` takes a `label` and, for user
> sessions, sends a bold label message and replies the sticker to it
> (`connectors/telegram/internal/telegram/sender.go:539-578` at `643829b`);
> the helper bot from 041 runs in the same process with its own dispatcher.
> Otherwise STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED — changes what is recorded as a sticker's copy for user
  sessions and adds a send path that depends on a second client
- **Depends on**: 041 (helper bot client)
- **Category**: feature
- **Planned at**: commit `643829b`, 2026-10-25

## Why this matters

Mirrored stickers carry their sender as a label, because Telegram stickers
take no caption. A bot session shows it as an inline button under the
sticker. A user session cannot attach buttons, so today it sends a separate
bold message ("🗣️: Alice", "(Sticker)") and replies the sticker to it. That
doubles every sticker into two messages, and the core only records the
sticker as the copy, so deleting the mirrored sticker leaves the label
behind.

With 041's helper bot in the process, a user session can send the sticker as
the helper bot's inline result, and an inline result **can** carry the same
label button bot sessions use. One message, labelled the same way on both
session kinds.

## Design decisions (settled with the owner — do not revisit)

| # | Decision |
|---|---|
| 1 | User-session stickers go through the helper bot: the helper uploads the sticker, answers an inline query with a sticker result carrying the label button (`wuphf:label`), and the user session sends that result with `messages.sendInlineBotResult`. |
| 2 | The sent sticker is the recorded copy. There is no separate label message. |
| 3 | Presses on the label are answered silently by the **helper bot** (the bot that owns the button). |
| 4 | When the helper bot is not configured, down, or slow, fall back to today's bold label plus the sticker as a reply to it. |
| 5 | This is a separate plan from the panel (041) and depends on it. |

### Decisions made by this plan (confirmed by the owner 2026-10-25)

| # | Decision | Why |
|---|---|---|
| F1 | The inline route has a **5 second** budget (upload, query, send); past it, fall back (decision 4). | A connector delivers in order (`config.example.toml`: later messages wait behind a sticker); a stuck helper must not stall the chat. |
| F2 | Inline queries for stickers use the same single-use nonce and user check as 041's F2, in a separate nonce map. | Same threat: anyone can type `@helperbot <text>`. |
| F3 | Unlabelled stickers (owner's own direct sends) keep using the user session's normal upload. | No button needed; one fewer moving part. |

## Current state

- `connectors/telegram/internal/telegram/sender.go:530-578` — `sendSticker`:

  ```go
  if label != "" {
      if s.bot {
          builder = builder.Markup(markup.InlineRow(markup.Callback(label, []byte(labelCallbackData))))
      } else {
          updates, err := builder.StyledText(ctx, styling.Bold(label), styling.Italic("\n(Sticker)"))
          …
          builder = builder.Reply(labelID)
      }
  }
  if attachment.GetAnimated() {
      if webm, err := s.animatedStickerWebM(ctx, attachment, data); err == nil {
          … UploadedDocument(file).MIME("video/webm").Filename("sticker.webm").UploadedSticker() …
      }
  }
  return builder.Upload(message.FromBytes("sticker.webp", data)).Sticker(ctx)
  ```
- `sender.go:98` — `SendMessage` calls `sendAttachment(…, senderLabel(...), …)`,
  and `:107-111` records `sentMessageID(updates)` as the copy.
- `sender.go:265-285` — `labelCallbackData = "wuphf:label"`, `senderLabel`.
- `connector.go:60-69` — the main client answers `wuphf:label` presses for
  bot sessions.
- `sticker_label_test.go` — `recordingInvoker`; tests
  `TestBotLabelsStickerWithInlineButton`,
  `TestUserSessionLabelsStickerWithRepliedLabelMessage`,
  `TestUnlabelledStickerSendsOnlyTheSticker`, `TestSenderLabel`.
- gotd v0.144.0 (verified in the module cache): `MessagesUploadMedia`,
  `MessagesGetInlineBotResults`, `MessagesSendInlineBotResult` (has
  `HideVia`, `ReplyTo`), `MessagesSetInlineBotResults`,
  `InputBotInlineResultDocument` (`Type`, `Document`, `SendMessage`) and
  `InputBotInlineMessageMediaAuto` (has `ReplyMarkup`), dispatcher hook
  `OnBotInlineQuery`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Telegram tests | `cd connectors/telegram && go vet ./... && go test -race ./...` | exit 0 |

## Scope

**In scope**: `connectors/telegram/internal/telegram/sender.go`,
`sticker_inline.go` (create), `sticker_label_test.go`, `connector.go`
(hand the helper to the sender), `connectors/telegram/README.md`.

**Out of scope**: the core; the panel (041) beyond reusing its helper client;
other attachment kinds (they have captions).

## Git workflow

- Commit on the current branch. Commits:
  1. `feat(telegram): send labelled user-session stickers through the helper bot`
  2. `docs(telegram): describe inline sticker labels`
- Do NOT push.

## Steps

### Step 0: Baseline

`cd connectors/telegram && go test -race ./...` → exit 0; record the count.

### Step 1: The helper's sticker path

Create `sticker_inline.go` with an `inlineStickers` type owned by the helper
bot (built in `connector.go` next to 041's panel helper, `nil` when there is
no helper):

1. `Send(ctx, peer, replyTo, data []byte, animated bool, label string) (tg.UpdatesClass, error)`
   on the **user** client's side:
   1. Upload the sticker through the **helper** client with
      `MessagesUploadMedia` (`InputMediaUploadedDocument`, WebM/VP9 for
      animated as `sendSticker` does today, WebP otherwise, with a
      `DocumentAttributeSticker`), giving a helper-owned `Document`.
   2. Store `{document, label}` under a fresh nonce (F2).
   3. `MessagesGetInlineBotResults(helper, peer, query: nonce)` from the user
      client.
   4. `MessagesSendInlineBotResult(peer, queryID, resultID, ReplyTo: replyTo)`
      from the user client; return its updates.
2. Helper `OnBotInlineQuery`: when the nonce is a pending sticker and the
   querying user is the user session, answer with one
   `InputBotInlineResultDocument{Type: "sticker", Document: <input document>,
   SendMessage: &InputBotInlineMessageMediaAuto{ReplyMarkup: label row}}`,
   `CacheTime: 0`, `Private: true`, and consume the nonce. 041's panel
   queries keep their own branch.
3. Helper `OnBotCallbackQuery` / `OnInlineBotCallbackQuery` for
   `labelCallbackData`: answer with no text (decision 3).

Which peer the **bot** uses for `MessagesUploadMedia` is not settled by the
docs at hand: try `InputPeerSelf` first, then the user session's
`InputPeerUser` as the bot sees it from the inline query's users. Record what
works in a comment.

**Verify**: tests 1–4 below pass.

### Step 2: Use it from `sendSticker`

In the `!s.bot` branch: when `label != ""` and the sender has an
`inlineStickers`, call `Send` under `context.WithTimeout(ctx, 5*time.Second)`
(F1). On success return its updates (the copy is the sticker, decision 2).
On any error, log at warn with the reason and run today's bold-label code
unchanged (decision 4). `label == ""` keeps today's path (F3). The animated
conversion (`animatedStickerWebM`) runs once, before choosing the route.

**Verify**: tests 5–7 pass, and the four existing sticker tests still pass
(the user-session one now runs with no helper and covers the fallback).

Commit 1.

### Step 3: Document

`README.md` "Settings panel" or a "Stickers" note: with
`WUPHF_TELEGRAM_PANEL_BOT_TOKEN` set, user-session stickers carry their label
as a button, Telegram shows "via @<helper bot>" under them, and without the
helper they fall back to a label message.

Commit 2.

### Step 4: Full gate

`cd connectors/telegram && gofmt -l . && go vet ./... && go test -race ./...`
→ no files listed, exit 0.

## Test plan

In `sticker_label_test.go` (recording invokers for both clients):

1. `TestHelperAnswersStickerQueryWithLabelButton` — the inline answer is one
   sticker document result whose `ReplyInlineMarkup` button has the label
   text and `wuphf:label` data.
2. `TestHelperIgnoresUnknownNonceOrOtherUser` (F2).
3. `TestNonceIsSingleUse`.
4. `TestHelperAnswersLabelPressSilently`.
5. `TestUserSessionStickerGoesThroughHelper` — requests in order: helper
   upload, user get-inline-results, user send-inline-result; no separate
   label message; the returned id is the sent sticker's.
6. `TestUserSessionStickerFallsBackWhenHelperFails` — helper upload errors;
   the label message and replied sticker are sent as today.
7. `TestUserSessionStickerFallsBackOnTimeout` — the helper blocks; the
   fallback runs within the budget (use a context deadline in the fake, not
   a sleep).

## Done criteria

- [ ] Tests 1–7 exist and pass with `-race`; the four existing sticker tests
      pass unchanged
- [ ] A delete of a mirrored user-session sticker leaves nothing behind when
      the helper route was used (test 5 asserts one message)
- [ ] `plans/README.md` row updated; F1–F3 recorded

## STOP conditions

- Neither `InputPeerSelf` nor the user's peer lets the helper bot upload a
  sticker document with `MessagesUploadMedia`.
- `sendInlineBotResult` with a sticker document result arrives as a document
  rather than a sticker in a live test.
- The fallback cannot be reached without a sleep-based test.

## Maintenance notes

- The visible "via @<helper bot>" line is Telegram's; `HideVia` is ignored
  for ordinary bots.
- If the helper is down for long, every labelled sticker pays the 5 second
  budget; a circuit breaker (skip the helper for a minute after a failure)
  is the obvious next step if that shows up.
