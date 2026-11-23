package setup

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"github.com/akshettrj/wuphf/connectors/shared/sessionlock"
	"github.com/propheci/botconf/go/tui"
)

// Account is a connector's account as its setup's actions use it, opened on
// the saved config.
type Account struct {
	ConnectorID string
	// Platform names the binary (wuphf-<platform>) and the directory file.
	Platform string
	// Session is the session the connector locks while it runs (its session
	// database, its cookies file).
	Session string
	// Link links the account, asking through the screen's session, and gives
	// a notice ("Linked as …"). An account linked already says so.
	Link func(ctx context.Context, screen tui.Session) (string, error)
	// Chats are the linked account's chats, with the ids the connector
	// reports to the core (chatdir.ChatRef); an error when it is not linked.
	// Nil when the account cannot list them (a Telegram bot).
	Chats func(ctx context.Context, screen tui.Session) ([]chatdir.Chat, error)
	// Close releases what the account opened; nil for nothing.
	Close func()
}

// OpenAccount opens a connector's account on a candidate (the saved files).
type OpenAccount func(tui.Candidate) (*Account, error)

// AccountActions are a connector's setup actions: Link account (link, then
// fetch its chats into the chat directory) and Refresh chats (fetch again).
// dir is the chat directory (chatdir.Dir()).
func AccountActions(dir string, open OpenAccount) []tui.Action {
	return []tui.Action{
		{
			Title: "Link account",
			Description: "Link this connector's account here, so the connector starts linked, then fetch its " +
				"chats into the chat directory: the core's and misc's setup offer them by name. Uses the saved " +
				"config; the connector must not be running.",
			Run: func(screen tui.Session, candidate tui.Candidate) (string, error) {
				return withAccount(screen, candidate, open, func(ctx context.Context, account *Account) (string, error) {
					notice, err := account.Link(ctx, screen)
					if err != nil {
						return "", err
					}
					if account.Chats == nil {
						return notice, nil
					}
					saved, err := saveChats(ctx, screen, dir, account)
					if err != nil {
						return "", fmt.Errorf("%s, but fetching its chats failed: %w", notice, err)
					}
					return notice + "; " + saved, nil
				})
			},
		},
		{
			Title: "Refresh chats",
			Description: "Fetch the linked account's chats into the chat directory again (new groups, renamed " +
				"chats). The connector must not be running.",
			Run: func(screen tui.Session, candidate tui.Candidate) (string, error) {
				return withAccount(screen, candidate, open, func(ctx context.Context, account *Account) (string, error) {
					if account.Chats == nil {
						return "", errors.New("This account cannot list its chats.")
					}
					return saveChats(ctx, screen, dir, account)
				})
			},
		},
	}
}

// withAccount opens the account and locks its session for run, with a
// context the screen's Esc cancels.
func withAccount(screen tui.Session, candidate tui.Candidate, open OpenAccount,
	run func(context.Context, *Account) (string, error)) (string, error) {
	account, err := open(candidate)
	if err != nil {
		return "", err
	}
	if account.Close != nil {
		defer account.Close()
	}
	lock, err := sessionlock.Acquire(account.Session)
	if errors.Is(err, sessionlock.ErrHeld) {
		return "", fmt.Errorf("wuphf-%s is running on %s: stop it, then try again", account.Platform, account.Session)
	}
	if err != nil {
		return "", err
	}
	defer lock.Release()
	ctx, cancel := cancelledBy(screen)
	defer cancel()
	notice, err := run(ctx, account)
	if err != nil && screen.Cancelled() {
		return "", errors.New("Stopped.")
	}
	return notice, err
}

// cancelledBy is a context cancelled when the user stops the action.
func cancelledBy(screen tui.Session) (context.Context, context.CancelFunc) {
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		ticker := time.NewTicker(100 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if screen.Cancelled() {
					cancel()
					return
				}
			}
		}
	}()
	return ctx, cancel
}

// saveChats fetches the account's chats and writes its directory file.
func saveChats(ctx context.Context, screen tui.Session, dir string, account *Account) (string, error) {
	if dir == "" {
		return "", fmt.Errorf("no chat directory: set %s (or $XDG_STATE_HOME or $HOME)", chatdir.DirectoryEnv)
	}
	screen.Progress("Fetching the chats…")
	chats, err := account.Chats(ctx, screen)
	if err != nil {
		return "", err
	}
	directory := chatdir.New(account.ConnectorID, account.Platform, time.Now().UnixMilli(), chats)
	path, err := directory.Write(dir)
	if err != nil {
		return "", fmt.Errorf("write the chat directory: %w", err)
	}
	return fmt.Sprintf("%d chats saved to %s", len(chats), path), nil
}
