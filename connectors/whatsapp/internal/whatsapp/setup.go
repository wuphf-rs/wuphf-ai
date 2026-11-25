package whatsappconnector

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/propheci/botconf/go/tui"
	"github.com/rs/zerolog"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/chatdir"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/setup"
	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/appstate"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	waLog "go.mau.fi/whatsmeow/util/log"
)

// How long setup waits for WhatsApp: to connect, and after a new link for
// the contacts (the first app-state sync), which may take a while.
const (
	setupConnectTimeout  = 30 * time.Second
	setupContactsTimeout = 90 * time.Second
)

// SetupAccount is the connector's account for its setup's actions (see the
// shared setup package): linked by the configured login method on the setup
// screen, its chats the joined groups, the saved contacts and the followed
// channels. logger must not write to the terminal the screen is drawn on.
func SetupAccount(cfg config.File, logger zerolog.Logger) *setup.Account {
	account := &setupAccount{cfg: cfg, logger: logger}
	return &setup.Account{
		ConnectorID: cfg.ConnectorID,
		Platform:    "whatsapp",
		Session:     cfg.SessionFile(),
		Link:        account.link,
		Chats:       account.chats,
		Close:       account.close,
	}
}

type setupAccount struct {
	cfg       config.File
	logger    zerolog.Logger
	container *sqlstore.Container
	client    *whatsmeow.Client
	// connected and synced close on WhatsApp's Connected and the contacts'
	// app-state sync.
	connected, synced *latch
	// linkedNow: this action linked the device, so the contacts are still to
	// come.
	linkedNow bool
}

// latch is closed once.
type latch struct {
	once sync.Once
	done chan struct{}
}

func newLatch() *latch    { return &latch{done: make(chan struct{})} }
func (l *latch) release() { l.once.Do(func() { close(l.done) }) }

func (a *setupAccount) open(ctx context.Context) error {
	if a.client != nil {
		return nil
	}
	container, err := sqlstore.New(ctx, "sqlite3", a.cfg.SessionDatabase,
		waLog.Zerolog(a.logger.With().Str("component", "whatsmeow-db").Logger()))
	if err != nil {
		return fmt.Errorf("open the WhatsApp session database: %w", err)
	}
	// As the connector registers (see Run): before any pairing.
	useAndroidClientIdentity(a.cfg.ResolvedClientMode() == config.ClientModeAndroidBusiness)
	device, err := container.GetFirstDevice(ctx)
	if err != nil {
		container.Close()
		return fmt.Errorf("load the WhatsApp device: %w", err)
	}
	a.container = container
	a.client = whatsmeow.NewClient(device, waLog.Zerolog(a.logger.With().Str("component", "whatsmeow").Logger()))
	a.connected, a.synced = newLatch(), newLatch()
	a.client.AddEventHandler(func(raw any) {
		switch evt := raw.(type) {
		case *events.Connected:
			a.connected.release()
		case *events.AppStateSyncComplete:
			if evt.Name == appstate.WAPatchCriticalUnblockLow {
				a.synced.release()
			}
		}
	})
	return nil
}

func (a *setupAccount) close() {
	if a.client != nil {
		a.client.Disconnect()
	}
	if a.container != nil {
		a.container.Close()
	}
}

func (a *setupAccount) link(ctx context.Context, screen tui.Session) (string, error) {
	if err := a.open(ctx); err != nil {
		return "", err
	}
	if id := a.client.Store.ID; id != nil {
		screen.Say("This session is linked already.")
		if err := a.client.Connect(); err != nil {
			return "", fmt.Errorf("connect WhatsApp: %w", err)
		}
		return "Linked as +" + id.User, nil
	}
	method := a.cfg.ResolvedLoginMethod()
	if method == config.LoginMethodPairCode {
		screen.Say("Linking by pair code for +" + a.cfg.PhoneNumber + ": WhatsApp on the phone asks for it.")
	} else {
		screen.Say("Scan the QR code with the phone's WhatsApp.")
	}
	connector := New(a.cfg, a.logger)
	if err := connector.link(ctx, a.client, &setupCodes{screen: screen, method: method}); err != nil {
		return "", err
	}
	if a.client.Store.ID == nil {
		return "", errors.New("WhatsApp did not link the device")
	}
	a.linkedNow = true
	screen.Say("Linked.")
	return "Linked as +" + a.client.Store.ID.User, nil
}

func (a *setupAccount) chats(ctx context.Context, screen tui.Session) ([]chatdir.Chat, error) {
	if err := a.open(ctx); err != nil {
		return nil, err
	}
	if a.client.Store.ID == nil {
		return nil, errors.New("not linked yet: run Link account first")
	}
	if !a.client.IsConnected() {
		if err := a.client.Connect(); err != nil {
			return nil, fmt.Errorf("connect WhatsApp: %w", err)
		}
	}
	screen.Progress("Connecting to WhatsApp…")
	if err := wait(ctx, a.connected, setupConnectTimeout); err != nil {
		return nil, fmt.Errorf("WhatsApp did not connect: %w", err)
	}
	if a.linkedNow {
		if !waitWithProgress(ctx, screen, a.synced, setupContactsTimeout, "Waiting for WhatsApp to send the contacts") {
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			screen.Say("WhatsApp has not sent the contacts yet: the groups are saved, and Refresh chats later adds the contacts.")
		}
	}
	screen.Progress("Fetching the groups, contacts and channels…")
	groups, err := a.client.GetJoinedGroups(ctx)
	if err != nil {
		return nil, fmt.Errorf("fetch the groups: %w", err)
	}
	contacts, err := a.client.Store.Contacts.GetAllContacts(ctx)
	if err != nil {
		return nil, fmt.Errorf("read the contacts: %w", err)
	}
	newsletters, err := a.client.GetSubscribedNewsletters(ctx)
	if err != nil {
		screen.Say("The channels could not be fetched: " + err.Error())
	}
	identity := newIdentityResolver(a.client)
	return directoryChats(a.cfg.ConnectorID, groups, contacts, newsletters, func(jid types.JID) types.JID {
		return identity.Canonical(ctx, jid)
	}), nil
}

// wait waits for l, at most timeout.
func wait(ctx context.Context, l *latch, timeout time.Duration) error {
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case <-l.done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return fmt.Errorf("no answer in %s", humanDuration(timeout))
	}
}

// waitWithProgress waits for l, at most timeout, saying how long so far.
func waitWithProgress(ctx context.Context, screen tui.Session, l *latch, timeout time.Duration, doing string) bool {
	start := time.Now()
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	for {
		select {
		case <-l.done:
			return true
		case <-ctx.Done():
			return false
		case <-deadline.C:
			return false
		case <-ticker.C:
			screen.Progress(fmt.Sprintf("%s… %d s (at most %s)", doing, int(time.Since(start).Seconds()), humanDuration(timeout)))
		}
	}
}

// directoryChats are the account's chats as the connector reports them to
// the core (canonical turns a phone address into its LID when known): the
// joined groups (a community's as "Community › Group", the community
// itself as one, plan 085), the contacts saved in the address book (or businesses; the
// people only seen in groups are left out, being many) and the followed
// channels, by title.
func directoryChats(connectorID string, groups []*types.GroupInfo, contacts map[types.JID]types.ContactInfo,
	newsletters []*types.NewsletterMetadata, canonical func(types.JID) types.JID) []chatdir.Chat {
	var out []chatdir.Chat
	seen := map[string]bool{}
	add := func(jid types.JID, title string, kind chatdir.Kind) {
		id := canonical(jid.ToNonAD()).String()
		if seen[id] {
			return
		}
		seen[id] = true
		if strings.TrimSpace(title) == "" {
			title = jid.User
		}
		out = append(out, chatdir.Chat{Chat: chatdir.ChatRef(connectorID, id), Title: title, Kind: kind})
	}
	names := groupNames(groups)
	for _, group := range groups {
		kind := chatdir.Group
		if group.IsParent {
			kind = chatdir.Community
		}
		community := ""
		if parent := parentOf(group); !parent.IsEmpty() {
			community = names(parent)
		}
		add(group.JID, communityName(community, group.Name), kind)
	}
	for jid, contact := range contacts {
		if strings.TrimSpace(contact.FullName) == "" && strings.TrimSpace(contact.BusinessName) == "" {
			continue
		}
		add(jid, formatPersonName(jid, contact, ""), chatdir.Direct)
	}
	for _, newsletter := range newsletters {
		add(newsletter.ID, newsletter.ThreadMeta.Name.Text, chatdir.Channel)
	}
	slices.SortFunc(out, func(a, b chatdir.Chat) int {
		return cmp.Or(cmp.Compare(strings.ToLower(a.Title), strings.ToLower(b.Title)), cmp.Compare(a.Chat, b.Chat))
	})
	return out
}

// setupCodes shows a pairing's codes on the setup screen: each QR code as
// WhatsApp rotates it, or a pair code each round.
type setupCodes struct {
	screen tui.Session
	method string
}

func (s *setupCodes) QR(_ context.Context, code string, valid time.Duration) {
	if s.method != config.LoginMethodQR {
		return
	}
	s.screen.QR(code)
	s.screen.Progress(fmt.Sprintf("WhatsApp → Settings → Linked devices → Link a device. This code is valid for %s; "+
		"a new one follows.", humanDuration(valid)))
}

// PairCodeAsk asks for a pair code every round, prompting the phone: someone
// is at the setup screen waiting for it.
func (s *setupCodes) PairCodeAsk() (pairCodeAsk, bool) {
	if s.method != config.LoginMethodPairCode {
		return pairCodeAsk{}, false
	}
	return pairCodeAsk{promptPhone: true}, true
}

func (s *setupCodes) PairCode(_ context.Context, code string, _ pairCodeAsk) {
	s.screen.Say(fmt.Sprintf("Pair code %s: WhatsApp → Settings → Linked devices → Link a device → Link with phone "+
		"number instead. Valid for about %s; a new one follows.", code, humanDuration(pairCodeValidity)))
}

func (s *setupCodes) PairCodeFailed(err error) {
	s.screen.Say("WhatsApp gave no pair code (" + err.Error() + "); the next round asks again.")
}

func (s *setupCodes) FreshPending() bool { return false }
