package telegramconnector

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/gotd/td/session"
	gotd "github.com/gotd/td/telegram"
	"github.com/gotd/td/telegram/auth"
	"github.com/gotd/td/tg"
	render "github.com/propheci/botconf/go/render/telegram"
	botconf "github.com/propheci/botconf/go/settings"
	"github.com/rs/zerolog"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/functions"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/sessionlock"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/telegram/internal/config"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"golang.org/x/sync/errgroup"
	"golang.org/x/term"
)

type Connector struct {
	cfg         config.File
	credentials config.Credentials
	logger      zerolog.Logger
}

func New(cfg config.File, credentials config.Credentials, logger zerolog.Logger) Connector {
	return Connector{cfg: cfg, credentials: credentials, logger: logger}
}

func (c Connector) Run(ctx context.Context) error {
	// Held while the connector runs, so setup's actions never use the session
	// at the same time.
	lock, err := sessionlock.Acquire(c.cfg.SessionFile)
	if err != nil {
		return fmt.Errorf("lock the Telegram session (is another wuphf-telegram, or its setup, using it?): %w", err)
	}
	defer lock.Release()
	coreToken, err := coreclient.TokenFromEnv(c.cfg.CoreTokenEnv)
	if err != nil {
		return err
	}
	media, err := coreclient.DialMedia(c.cfg.CoreAddress, coreToken)
	if err != nil {
		return err
	}
	defer media.Close()
	var functionsToken string
	if c.cfg.FunctionsTokenEnv != "" {
		functionsToken = os.Getenv(c.cfg.FunctionsTokenEnv)
	}
	fns, err := functions.Dial(c.cfg.CoreAddress, functionsToken, c.cfg.ConnectorID)
	if err != nil {
		return err
	}
	defer fns.Close()
	// The runtime settings: declared to the core when registering, then
	// followed as the core pushes changes.
	runtime, err := settings.Connect(c.cfg.CoreAddress, coreToken, c.cfg.ConnectorID, c.cfg.Runtime.WithDefaults(), c.logger)
	if err != nil {
		return err
	}
	defer runtime.Close()
	go runtime.Follow(ctx, func(old, new config.Runtime) {
		if old.LogLevel != new.LogLevel {
			logging.SetLevel(new.LogLevel)
		}
	})
	converter := functions.MediaConverter{Functions: fns, Media: media, TimeoutFunc: func() time.Duration {
		return runtime.Live.Load().MediaConvertTimeout()
	}}
	var client *gotd.Client
	codec := NewMediaCodec(media, func(ctx context.Context, location tg.InputFileLocationClass) ([]byte, error) {
		var output bytes.Buffer
		if _, err := client.Download(location).Stream(ctx, &output); err != nil {
			return nil, err
		}
		return output.Bytes(), nil
	})
	peers := NewPeerCache()
	inputs := make(chan *wuphfpb.ClientFrame, 128)
	// The settings panel talks to the core's settings service with this
	// connector's token; the core checks every actor against owner_ids.
	panelClient, err := botconf.Dial(c.cfg.CoreAddress, botconf.WithBearer(coreToken))
	if err != nil {
		return err
	}
	defer panelClient.Close()
	panel := NewPanel(c.cfg.Panel.CommandName(), peers, c.logger)
	// Labelled stickers of a user session go through the same helper bot.
	stickers := NewInlineStickers(c.logger)
	// Owners can also open the panel by messaging the helper bot privately.
	chats := NewHelperChats(c.cfg.Panel.CommandName(), c.logger)
	dispatcher := NewNormalizer(peers, inputs, codec).WithPanel(panel).Dispatcher()
	// Presses of the settings panel's buttons (bot sessions). A bot also labels
	// stickers with an inline button (see sendSticker); pressing it does
	// nothing, but an unanswered press leaves the client spinning, so it is
	// answered at once with no notification.
	dispatcher.OnBotCallbackQuery(func(ctx context.Context, _ tg.Entities, update *tg.UpdateBotCallbackQuery) error {
		if taken, err := panel.HandleCallback(ctx, update); taken {
			return err
		}
		if string(update.Data) != labelCallbackData {
			return nil
		}
		_, err := client.API().MessagesSetBotCallbackAnswer(ctx, &tg.MessagesSetBotCallbackAnswerRequest{QueryID: update.QueryID})
		return err
	})
	client = gotd.NewClient(c.credentials.AppID, c.credentials.AppHash, gotd.Options{
		SessionStorage: &session.FileStorage{Path: c.cfg.SessionFile},
		UpdateHandler:  dispatcher,
	})
	// A user session with a panel bot token shows the settings panel through
	// a helper bot (runHelper), a second client in this process; either
	// session ending ends the connector, so reconnecting stays in one place.
	// The helper starts only once the main session is known to be a user
	// session (see serve).
	group, ctx := errgroup.WithContext(ctx)
	helperReady := make(chan *gotd.Client, 1)
	var helperOnce sync.Once
	startHelper := func() {
		helperOnce.Do(func() {
			group.Go(func() error { return c.runHelper(ctx, panel, stickers, chats, panelClient, helperReady) })
		})
	}
	group.Go(func() error {
		return client.Run(ctx, func(ctx context.Context) error {
			return c.serve(ctx, client, peers, panel, stickers, panelClient, helperReady, startHelper, func(ctx context.Context, bot bool) error {
				sender := NewSender(client.API(), peers, media, converter, runtime.Live, bot).WithInlineStickers(stickers)
				return coreclient.New(coreclient.Options{
					CoreAddress:         c.cfg.CoreAddress,
					Token:               coreToken,
					ConnectorID:         c.cfg.ConnectorID,
					Platform:            wuphfpb.Platform_PLATFORM_TELEGRAM,
					SupportsThreads:     c.cfg.SupportsThreads,
					SupportsTopicCreate: c.cfg.SupportsTopicCreate,
					IsPermanent:         telegramPermanent,
					Settings:            runtime.Declaration,
					Prober:              NewProber(client.API(), peers),
				}, c.logger).Run(ctx, sender, inputs)
			})
		})
	})
	return group.Wait()
}

// serve runs on the logged-in main session: it prepares peer addressing and
// the settings panel, then bridges messages with bridge until ctx ends.
func (c Connector) serve(
	ctx context.Context,
	client *gotd.Client,
	peers *PeerCache,
	panel *Panel,
	stickers *InlineStickers,
	panelClient *botconf.Client,
	helperReady <-chan *gotd.Client,
	startHelper func(),
	bridge func(ctx context.Context, bot bool) error,
) error {
	if err := c.authenticate(ctx, client); err != nil {
		return err
	}
	// The stored session decides what this connector is: a reused session
	// file may hold a bot or a user whatever the credentials say now, and
	// the session is what Telegram sees.
	self, err := client.Self(ctx)
	if err != nil {
		return fmt.Errorf("resolve Telegram session identity: %w", err)
	}
	if warning := sessionMismatch(c.cfg.SessionFile, c.credentials.BotToken != "", self); warning != "" {
		c.logger.Warn().Msg(warning)
	}
	if err := prepareAddressing(ctx, self, client.API(), peers, c.logger); err != nil {
		return err
	}
	c.logger.Info().Bool("bot", self.Bot).Str("account", accountName(self)).Msg("Telegram session connected")
	session := panelSession{self: self.ID, names: []string{self.Username}}
	if self.Bot {
		panel.startBot(panelClient, client.API(), session)
		if c.credentials.PanelBotToken != "" {
			c.logger.Info().Msg("the session is a bot, so it shows the settings panel itself; the panel bot token is not used")
		}
	} else {
		// Text panels until the helper bot (if any) is up.
		panel.startUser(panelClient, client.API(), render.NewInlinePanel(panelClient, client.API(), nil, nil, self.ID), session)
		switch {
		case c.credentials.PanelBotToken == "":
		case c.cfg.Panel.SessionFile == "":
			c.logger.Warn().Msg("a panel bot token is set but panel.session_file is not; the settings panel stays text")
		default:
			startHelper()
			go c.attachHelper(ctx, client.API(), panel, stickers, panelClient, session, helperReady)
		}
	}
	return bridge(ctx, self.Bot)
}

// prepareAddressing readies the peer cache for the session's kind. A user
// session primes access hashes from its dialog list; messages.getDialogs is
// user-only, so a bot instead looks up each peer it has not seen an update
// from on its first send (see UseBotAddressing).
func prepareAddressing(ctx context.Context, self *tg.User, api *tg.Client, peers *PeerCache, logger zerolog.Logger) error {
	if self.Bot {
		peers.UseBotAddressing(apiPeerResolver{api: api}, logger)
		return nil
	}
	return primePeers(ctx, api, peers)
}

// sessionMismatch explains a stored session whose kind contradicts the
// configured credentials, or returns "" when they agree. The stored session
// still wins: it is valid and is what Telegram sees, so the bridge keeps
// working, and the warning says how to switch.
func sessionMismatch(sessionFile string, hasBotToken bool, self *tg.User) string {
	switch {
	case hasBotToken && !self.Bot:
		return fmt.Sprintf("session file %q is logged in as the user account %s, but a bot token is configured; "+
			"running as that user. To run as the bot, stop the connector, delete %q and start it again",
			sessionFile, accountName(self), sessionFile)
	case !hasBotToken && self.Bot:
		return fmt.Sprintf("session file %q is logged in as the bot %s, but no bot token is configured, so a user session was expected; "+
			"running as that bot. To run as your account, stop the connector, delete %q, make sure WUPHF_TELEGRAM_BOT_TOKEN "+
			"and bot_token are unset, and start it again to log in with WUPHF_TELEGRAM_PHONE",
			sessionFile, accountName(self), sessionFile)
	default:
		return ""
	}
}

// accountName names a Telegram account for logs: @username, else its id.
func accountName(user *tg.User) string {
	if user.Username != "" {
		return "@" + user.Username
	}
	return strconv.FormatInt(user.ID, 10)
}

// attachHelper switches a user session's panel and labelled stickers to the
// helper bot once it has logged in, so both get buttons.
func (c Connector) attachHelper(ctx context.Context, api *tg.Client, panel *Panel, stickers *InlineStickers, panelClient *botconf.Client, session panelSession, helperReady <-chan *gotd.Client) {
	var helper *gotd.Client
	select {
	case helper = <-helperReady:
	case <-ctx.Done():
		return
	}
	helperSelf, err := helper.Self(ctx)
	if err != nil {
		c.logger.Warn().Err(err).Msg("settings panel: cannot resolve the helper bot; panels stay text")
		return
	}
	botUser, err := resolveHelperBot(ctx, api, helperSelf)
	if err != nil {
		c.logger.Warn().Err(err).Msg("settings panel: the user session cannot reach the helper bot; panels stay text")
		return
	}
	panel.startUser(panelClient, api, render.NewInlinePanel(panelClient, api, helper, botUser, session.self), session)
	panel.addNames(helperSelf.Username)
	stickers.attach(api, helper.API(), botUser, session.self)
	c.logger.Info().Str("helper", helperSelf.Username).Msg("settings panel uses the helper bot")
}

// resolveHelperBot is how the user session addresses the helper bot: its
// access hash as seen by the user session, found by username.
func resolveHelperBot(ctx context.Context, api *tg.Client, helper *tg.User) (tg.InputUserClass, error) {
	if helper.Username == "" {
		return nil, fmt.Errorf("the helper bot %d has no username", helper.ID)
	}
	resolved, err := api.ContactsResolveUsername(ctx, &tg.ContactsResolveUsernameRequest{Username: helper.Username})
	if err != nil {
		return nil, fmt.Errorf("resolve @%s: %w", helper.Username, err)
	}
	for _, class := range resolved.Users {
		if user, ok := class.(*tg.User); ok && user.ID == helper.ID {
			return &tg.InputUser{UserID: user.ID, AccessHash: user.AccessHash}, nil
		}
	}
	return nil, fmt.Errorf("@%s did not resolve to the helper bot", helper.Username)
}

// runHelper runs the helper bot a user session shows the settings panel
// through: it answers the panel's inline queries and its button presses, and
// serves the panel to owners in its own private chats. Once logged in it
// hands itself to the main session over ready.
func (c Connector) runHelper(ctx context.Context, panel *Panel, stickers *InlineStickers, chats *HelperChats, panelClient *botconf.Client, ready chan<- *gotd.Client) error {
	var helper *gotd.Client
	dispatcher := newHelperDispatcher(panel, stickers, chats, func() *tg.Client { return helper.API() })
	helper = gotd.NewClient(c.credentials.AppID, c.credentials.AppHash, gotd.Options{
		SessionStorage: &session.FileStorage{Path: c.cfg.Panel.SessionFile},
		UpdateHandler:  dispatcher,
	})
	return helper.Run(ctx, func(ctx context.Context) error {
		if err := authenticateBot(ctx, helper, c.credentials.PanelBotToken); err != nil {
			return fmt.Errorf("settings panel helper bot: %w", err)
		}
		helperSelf, err := helper.Self(ctx)
		if err != nil {
			return fmt.Errorf("settings panel helper bot: %w", err)
		}
		chats.start(panelClient, helper.API(), helperSelf.Username)
		// Best effort: the command still works when the menu cannot be set.
		if _, err := helper.API().BotsSetBotCommands(ctx, helperCommands(c.cfg.Panel.CommandName())); err != nil {
			c.logger.Debug().Err(err).Msg("could not set the helper bot's command menu")
		}
		c.logger.Info().Str("helper", helperSelf.Username).Str("command", "/"+c.cfg.Panel.CommandName()).
			Msg("Telegram panel helper bot connected; owners can message it the command privately")
		ready <- helper
		<-ctx.Done()
		return ctx.Err()
	})
}

// newHelperDispatcher routes the helper bot's updates: the settings panel's
// inline queries and presses first, then the inline stickers' queries.
// Anything else typed after @<helper bot> gets nothing, and every other press
// (a sticker's label button among them) is answered silently, since an
// unanswered press leaves the client spinning. Private messages and presses
// in the helper's own chats go to chats (when set); nothing the helper
// receives is mirrored.
func newHelperDispatcher(panel *Panel, stickers *InlineStickers, chats *HelperChats, api func() *tg.Client) tg.UpdateDispatcher {
	dispatcher := tg.NewUpdateDispatcher()
	if chats != nil {
		dispatcher.OnNewMessage(func(ctx context.Context, entities tg.Entities, update *tg.UpdateNewMessage) error {
			_, err := chats.HandleMessage(ctx, entities, update.Message)
			return err
		})
	}
	dispatcher.OnBotCallbackQuery(func(ctx context.Context, entities tg.Entities, update *tg.UpdateBotCallbackQuery) error {
		if chats != nil {
			if taken, err := chats.HandleCallback(ctx, entities, update); taken {
				return err
			}
		}
		_, err := api().MessagesSetBotCallbackAnswer(ctx, &tg.MessagesSetBotCallbackAnswerRequest{QueryID: update.QueryID})
		return err
	})
	dispatcher.OnBotInlineQuery(func(ctx context.Context, entities tg.Entities, update *tg.UpdateBotInlineQuery) error {
		if taken, err := panel.HandleInlineQuery(ctx, update); taken {
			return err
		}
		if taken, err := stickers.HandleInlineQuery(ctx, api(), entities, update); taken {
			return err
		}
		_, err := api().MessagesSetInlineBotResults(ctx, &tg.MessagesSetInlineBotResultsRequest{
			QueryID:   update.QueryID,
			CacheTime: 0,
			Private:   true,
		})
		return err
	})
	dispatcher.OnInlineBotCallbackQuery(func(ctx context.Context, _ tg.Entities, update *tg.UpdateInlineBotCallbackQuery) error {
		if taken, err := panel.HandleInlineCallback(ctx, update); taken {
			return err
		}
		_, err := api().MessagesSetBotCallbackAnswer(ctx, &tg.MessagesSetBotCallbackAnswerRequest{QueryID: update.QueryID})
		return err
	})
	return dispatcher
}

// primePeers seeds the peer cache from the account's dialog list so the core can
// dispatch to a configured chat (e.g. a mirror container) before any update from
// it has been observed. MTProto requires an access hash to address a user or
// channel, and the dialog list is the cheapest source of hashes at startup.
func primePeers(ctx context.Context, api *tg.Client, peers *PeerCache) error {
	dialogs, err := api.MessagesGetDialogs(ctx, &tg.MessagesGetDialogsRequest{
		OffsetPeer: &tg.InputPeerEmpty{},
		Limit:      100,
	})
	if err != nil {
		return fmt.Errorf("prime Telegram peer cache: %w", err)
	}
	switch value := dialogs.(type) {
	case *tg.MessagesDialogs:
		peers.ObserveClasses(value.Users, value.Chats)
	case *tg.MessagesDialogsSlice:
		peers.ObserveClasses(value.Users, value.Chats)
	}
	return nil
}

func (c Connector) authenticate(ctx context.Context, client *gotd.Client) error {
	if c.credentials.BotToken != "" {
		return authenticateBot(ctx, client, c.credentials.BotToken)
	}
	if authorized(ctx, client) {
		return nil
	}

	codePrompt := auth.CodeAuthenticatorFunc(func(_ context.Context, _ *tg.AuthSentCode) (string, error) {
		fmt.Fprint(os.Stderr, "Telegram authentication code: ")
		code, err := bufio.NewReader(os.Stdin).ReadString('\n')
		if err != nil {
			return "", err
		}
		return strings.TrimSpace(code), nil
	})

	authenticator := passwordPrompt{
		UserAuthenticator: auth.Constant(c.credentials.Phone, c.credentials.Password, codePrompt),
		configured:        c.credentials.Password,
	}
	if err := auth.NewFlow(authenticator, auth.SendCodeOptions{}).Run(ctx, client.Auth()); err != nil {
		return fmt.Errorf("authenticate Telegram user: %w", err)
	}
	return nil
}

// passwordPrompt answers the 2FA password challenge: it returns the configured
// password when one is set, and otherwise prompts for it interactively without
// echoing. Phone, code, sign-up, and ToS handling are inherited unchanged.
type passwordPrompt struct {
	auth.UserAuthenticator
	configured string
}

func (p passwordPrompt) Password(_ context.Context) (string, error) {
	if p.configured != "" {
		return p.configured, nil
	}
	fmt.Fprint(os.Stderr, "Telegram 2FA password: ")
	secret, err := term.ReadPassword(int(os.Stdin.Fd()))
	fmt.Fprintln(os.Stderr)
	if err != nil {
		return "", fmt.Errorf("read Telegram 2FA password: %w", err)
	}
	return strings.TrimSpace(string(secret)), nil
}

// authorized says whether the stored session is still logged in. Telegram
// heavily rate-limits auth calls (FLOOD_WAIT), and bot ImportBotAuthorization
// is not self-guarded, so re-authenticating on every restart eventually locks
// the account out; a stored session is reused instead.
func authorized(ctx context.Context, client *gotd.Client) bool {
	status, err := client.Auth().Status(ctx)
	return err == nil && status.Authorized
}

// authenticateBot logs client in as the bot with token, unless its stored
// session already is.
func authenticateBot(ctx context.Context, client *gotd.Client, token string) error {
	if authorized(ctx, client) {
		return nil
	}
	if _, err := client.Auth().Bot(ctx, token); err != nil {
		return fmt.Errorf("authenticate Telegram bot: %w", err)
	}
	return nil
}
