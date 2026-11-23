package telegramconnector

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"
	"strconv"
	"strings"

	"github.com/akshettrj/wuphf/connectors/shared/chatdir"
	"github.com/akshettrj/wuphf/connectors/shared/setup"
	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	"github.com/gotd/td/session"
	gotd "github.com/gotd/td/telegram"
	"github.com/gotd/td/telegram/auth"
	"github.com/gotd/td/telegram/message/peer"
	"github.com/gotd/td/telegram/query"
	"github.com/gotd/td/tg"
	"github.com/propheci/botconf/go/tui"
)

// SetupAccount is the connector's account for its setup's actions (see the
// shared setup package). A user session logs in on the setup screen (the
// phone, unless WUPHF_TELEGRAM_PHONE is set, the code, the 2FA password
// unless WUPHF_TELEGRAM_PASSWORD is set), its chats the dialogs. A bot's
// token is only checked: a bot cannot list its chats.
func SetupAccount(cfg config.File, credentials config.Credentials) *setup.Account {
	account := setupAccount{cfg: cfg, credentials: credentials}
	out := &setup.Account{
		ConnectorID: cfg.ConnectorID,
		Platform:    "telegram",
		Session:     cfg.SessionFile,
		Link:        account.link,
	}
	if credentials.BotToken == "" {
		out.Chats = account.chats
	}
	return out
}

type setupAccount struct {
	cfg         config.File
	credentials config.Credentials
}

// run runs f on a client of the session file (gotd logs nowhere by default,
// the terminal being the setup screen).
func (a setupAccount) run(ctx context.Context, f func(context.Context, *gotd.Client) error) error {
	client := gotd.NewClient(a.credentials.AppID, a.credentials.AppHash, gotd.Options{
		SessionStorage: &session.FileStorage{Path: a.cfg.SessionFile},
	})
	return client.Run(ctx, func(ctx context.Context) error { return f(ctx, client) })
}

func (a setupAccount) link(ctx context.Context, screen tui.Session) (string, error) {
	var notice string
	err := a.run(ctx, func(ctx context.Context, client *gotd.Client) error {
		if token := a.credentials.BotToken; token != "" {
			screen.Progress("Checking the bot token…")
			if err := authenticateBot(ctx, client, token); err != nil {
				return err
			}
			self, err := client.Self(ctx)
			if err != nil {
				return err
			}
			notice = "The bot " + accountName(self) + " works (a bot cannot list its chats: name them by id)"
			return nil
		}
		if authorized(ctx, client) {
			screen.Say("This session is logged in already.")
		} else {
			screen.Progress("Logging in…")
			authenticator := setupAuthenticator{screen: screen, phone: a.credentials.Phone, password: a.credentials.Password}
			if err := auth.NewFlow(authenticator, auth.SendCodeOptions{}).Run(ctx, client.Auth()); err != nil {
				return fmt.Errorf("log in to Telegram: %w", err)
			}
		}
		self, err := client.Self(ctx)
		if err != nil {
			return err
		}
		notice = "Logged in as " + userTitle(self)
		return nil
	})
	return notice, err
}

func (a setupAccount) chats(ctx context.Context, screen tui.Session) ([]chatdir.Chat, error) {
	var out []chatdir.Chat
	err := a.run(ctx, func(ctx context.Context, client *gotd.Client) error {
		if !authorized(ctx, client) {
			return errors.New("not logged in yet: run Link account first")
		}
		dialogs := query.GetDialogs(client.API()).BatchSize(100).Iter()
		for dialogs.Next(ctx) {
			elem := dialogs.Value()
			if chat, ok := dialogChat(a.cfg.ConnectorID, elem.Dialog, elem.Entities); ok {
				out = append(out, chat)
				if len(out)%100 == 0 {
					screen.Progress(fmt.Sprintf("Fetched %d chats…", len(out)))
				}
			}
		}
		return dialogs.Err()
	})
	if err != nil {
		return nil, err
	}
	slices.SortFunc(out, func(a, b chatdir.Chat) int {
		return cmp.Or(cmp.Compare(strings.ToLower(a.Title), strings.ToLower(b.Title)), cmp.Compare(a.Chat, b.Chat))
	})
	return out, nil
}

// dialogChat is a dialog as the directory holds it, with the chat id the
// connector reports to the core (formatPeer); false for a folder.
func dialogChat(connectorID string, dialog tg.DialogClass, entities peer.Entities) (chatdir.Chat, bool) {
	d, ok := dialog.(*tg.Dialog)
	if !ok {
		return chatdir.Chat{}, false
	}
	var title string
	var kind chatdir.Kind
	switch p := d.Peer.(type) {
	case *tg.PeerUser:
		kind, title = chatdir.Direct, strconv.FormatInt(p.UserID, 10)
		if user, ok := entities.Users()[p.UserID]; ok {
			title = userTitle(user)
		}
	case *tg.PeerChat:
		kind = chatdir.Group
		if chat, ok := entities.Chats()[p.ChatID]; ok {
			title = chat.Title
		}
	case *tg.PeerChannel:
		kind = chatdir.Group
		if channel, ok := entities.Channels()[p.ChannelID]; ok {
			title = channel.Title
			if channel.Broadcast {
				kind = chatdir.Channel
			}
		}
	default:
		return chatdir.Chat{}, false
	}
	id := formatPeer(d.Peer)
	if strings.TrimSpace(title) == "" {
		title = id
	}
	return chatdir.Chat{Chat: chatdir.ChatRef(connectorID, id), Title: title, Kind: kind}, true
}

// userTitle is how a person reads in a chat list.
func userTitle(user *tg.User) string {
	switch {
	case user.Self:
		return "Saved Messages"
	case user.Deleted:
		return "Deleted account"
	}
	if name := strings.TrimSpace(user.FirstName + " " + user.LastName); name != "" {
		if user.Username != "" {
			return name + " (@" + user.Username + ")"
		}
		return name
	}
	return accountName(user)
}

// setupAuthenticator answers the login on the setup screen.
type setupAuthenticator struct {
	screen tui.Session
	// phone and password are WUPHF_TELEGRAM_PHONE and _PASSWORD, asked for
	// when empty.
	phone, password string
}

func (s setupAuthenticator) Phone(context.Context) (string, error) {
	if s.phone != "" {
		return s.phone, nil
	}
	return s.ask("The account's phone number, international (+15551234567):", false)
}

func (s setupAuthenticator) Code(context.Context, *tg.AuthSentCode) (string, error) {
	return s.ask("The login code Telegram sent:", false)
}

func (s setupAuthenticator) Password(context.Context) (string, error) {
	if s.password != "" {
		return s.password, nil
	}
	return s.ask("The account's two-step verification password:", true)
}

func (setupAuthenticator) AcceptTermsOfService(context.Context, tg.HelpTermsOfService) error {
	return errors.New("Telegram asks to accept new terms of service: accept them in a Telegram app, then link again")
}

func (setupAuthenticator) SignUp(context.Context) (auth.UserInfo, error) {
	return auth.UserInfo{}, errors.New("this phone number has no Telegram account")
}

func (s setupAuthenticator) ask(question string, secret bool) (string, error) {
	ask := s.screen.Ask
	if secret {
		ask = s.screen.AskSecret
	}
	answer, ok := ask(question)
	if !ok {
		return "", errors.New("stopped")
	}
	return strings.TrimSpace(answer), nil
}
