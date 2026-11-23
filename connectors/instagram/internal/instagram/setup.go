package instagramconnector

import (
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"slices"
	"strconv"
	"strings"

	"github.com/akshettrj/wuphf/connectors/instagram/internal/config"
	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"github.com/akshettrj/wuphf/connectors/shared/setup"
	"github.com/propheci/botconf/go/tui"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/instameow"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/util/exhttp"
)

// SetupAccount is the connector's account for its setup's actions (see the
// shared setup package): the session cookies, exported from a logged-in
// browser and pasted or named on the setup screen, written to cookies_file
// and checked by loading the account; its chats the inbox's threads.
func SetupAccount(cfg config.File) *setup.Account {
	account := &setupAccount{cfg: cfg}
	return &setup.Account{
		ConnectorID: cfg.ConnectorID,
		Platform:    "instagram",
		Session:     cfg.SessionFile,
		Link:        account.link,
		Chats:       account.chats,
		Close:       account.close,
	}
}

type setupAccount struct {
	cfg    config.File
	client *instameow.Client
	// mailbox is the inbox the account loaded with; loaded, that it did.
	mailbox *slidetypes.Mailbox
	loaded  bool
}

func (a *setupAccount) link(ctx context.Context, screen tui.Session) (string, error) {
	jar, err := loadCookies(a.cfg)
	question := "Paste the session cookies (JSON) exported from a logged-in browser, or the path of a file holding them:"
	if err == nil && jar.IsLoggedIn() {
		question = "Cookies are set already. Enter to check them, or paste new ones (JSON) or a file's path:"
	}
	answer, ok := screen.Ask(question)
	if !ok {
		return "", errors.New("stopped")
	}
	switch answer = strings.TrimSpace(answer); {
	case answer != "":
		if jar, err = readCookies(answer); err != nil {
			return "", err
		}
	case err != nil:
		return "", errors.New("no cookies given, and none saved yet")
	}
	if !jar.IsLoggedIn() {
		return "", fmt.Errorf("the cookies hold no session (missing %v)", jar.GetMissingCookieNames())
	}
	screen.Progress("Loading the Instagram account…")
	if err := a.load(ctx, jar); err != nil {
		return "", err
	}
	if answer != "" {
		// The cookies the account loaded with, refreshed: cookies_file for
		// a later setup, the session file for the connector (it is read
		// first).
		if err := persistCookies(a.cfg.CookiesFile, a.client.GetCookies()); err != nil {
			return "", err
		}
	}
	return "Logged in (account " + strconv.FormatInt(a.client.GetOwnFBID(), 10) + ")", nil
}

// readCookies reads cookies pasted as JSON, or from the file answer names.
func readCookies(answer string) (*cookies.Cookies, error) {
	data := []byte(answer)
	if !strings.HasPrefix(answer, "{") {
		var err error
		if data, err = os.ReadFile(answer); err != nil {
			return nil, fmt.Errorf("read the cookies: %w", err)
		}
	}
	jar := &cookies.Cookies{Platform: types.Instagram}
	if err := json.Unmarshal(data, jar); err != nil {
		return nil, fmt.Errorf("the cookies are not a JSON map of cookie names to values: %w", err)
	}
	return jar, nil
}

// load loads the account on jar, keeping its inbox.
func (a *setupAccount) load(ctx context.Context, jar *cookies.Cookies) error {
	a.client = instameow.NewClient(instameow.ClientParams{
		Cookies: jar,
		// The terminal is the setup screen.
		Log:           zerolog.Nop(),
		Settings:      exhttp.SensibleClientSettings,
		DisableTyping: true,
	})
	_, mailbox, err := a.client.LoadIndex(ctx)
	if err != nil {
		return fmt.Errorf("Instagram refused the cookies: %w", err)
	}
	a.mailbox, a.loaded = mailbox, true
	return nil
}

func (a *setupAccount) chats(ctx context.Context, _ tui.Session) ([]chatdir.Chat, error) {
	if a.client == nil {
		jar, err := loadCookies(a.cfg)
		if err != nil || !jar.IsLoggedIn() {
			return nil, errors.New("not logged in yet: run Link account first")
		}
		if err := a.load(ctx, jar); err != nil {
			return nil, err
		}
	}
	threads := NewThreadDirectory(a.client, a.client.GetOwnFBID())
	threads.RecordMailbox(a.mailbox)
	return directoryChats(a.cfg.ConnectorID, threads.Threads()), nil
}

// directoryChats are the inbox's threads as the directory holds them, by
// title.
func directoryChats(connectorID string, threads []ListedThread) []chatdir.Chat {
	out := make([]chatdir.Chat, 0, len(threads))
	for _, thread := range threads {
		kind := chatdir.Direct
		if thread.IsGroup {
			kind = chatdir.Group
		}
		title := cmp.Or(strings.TrimSpace(thread.Title), thread.ChatID)
		out = append(out, chatdir.Chat{Chat: chatdir.ChatRef(connectorID, thread.ChatID), Title: title, Kind: kind})
	}
	slices.SortFunc(out, func(a, b chatdir.Chat) int {
		return cmp.Or(cmp.Compare(strings.ToLower(a.Title), strings.ToLower(b.Title)), cmp.Compare(a.Chat, b.Chat))
	})
	return out
}

// close keeps the refreshed cookies in the session file, as the connector
// does on shutdown; the connector reads it first.
func (a *setupAccount) close() {
	if a.client == nil {
		return
	}
	a.client.Disconnect()
	if !a.loaded {
		return
	}
	// Nothing reads a failure here; the session file stays as it was.
	_ = persistCookies(a.cfg.SessionFile, a.client.GetCookies())
}
