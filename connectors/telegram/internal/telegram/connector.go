package telegramconnector

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	coreclient "github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/shared/functions"
	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/akshettrj/wuphf/connectors/shared/settings"
	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/session"
	gotd "github.com/gotd/td/telegram"
	"github.com/gotd/td/telegram/auth"
	"github.com/gotd/td/tg"
	render "github.com/propheci/botconf/go/render/telegram"
	botconf "github.com/propheci/botconf/go/settings"
	"github.com/rs/zerolog"
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
	group, ctx := errgroup.WithContext(ctx)
	helperReady := make(chan *gotd.Client, 1)
	if c.credentials.BotToken == "" && c.credentials.PanelBotToken != "" {
		group.Go(func() error { return c.runHelper(ctx, panel, helperReady) })
	}
	group.Go(func() error {
		return client.Run(ctx, func(ctx context.Context) error {
			return c.serve(ctx, client, peers, panel, panelClient, helperReady, func(ctx context.Context) error {
				sender := NewSender(client.API(), peers, media, converter, runtime.Live, c.credentials.BotToken != "")
				return coreclient.New(coreclient.Options{
					CoreAddress:         c.cfg.CoreAddress,
					Token:               coreToken,
					ConnectorID:         c.cfg.ConnectorID,
					Platform:            wuphfpb.Platform_PLATFORM_TELEGRAM,
					SupportsThreads:     c.cfg.SupportsThreads,
					SupportsTopicCreate: c.cfg.SupportsTopicCreate,
					IsPermanent:         telegramPermanent,
					Settings:            runtime.Declaration,
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
	panelClient *botconf.Client,
	helperReady <-chan *gotd.Client,
	bridge func(context.Context) error,
) error {
	if err := c.authenticate(ctx, client); err != nil {
		return err
	}
	// Cold-start peer addressing differs by session kind. A user session
	// primes access hashes from its dialog list; messages.getDialogs is
	// user-only, so a bot instead relies on Telegram accepting a zero hash
	// for peers it already has access to.
	if c.credentials.BotToken != "" {
		peers.UseBotAddressing()
	} else if err := primePeers(ctx, client.API(), peers); err != nil {
		return err
	}
	c.logger.Info().Msg("Telegram session connected")
	self, err := client.Self(ctx)
	if err != nil {
		return fmt.Errorf("resolve Telegram session identity: %w", err)
	}
	session := panelSession{self: self.ID, names: []string{self.Username}}
	if self.Bot {
		panel.startBot(panelClient, client.API(), session)
	} else {
		// Text panels until the helper bot (if any) is up.
		panel.startUser(panelClient, client.API(), render.NewInlinePanel(panelClient, client.API(), nil, nil, self.ID), session)
		go c.attachHelper(ctx, client.API(), panel, panelClient, session, helperReady)
	}
	return bridge(ctx)
}

// attachHelper switches a user session's panel to the helper bot once it has
// logged in, so the panel gets buttons.
func (c Connector) attachHelper(ctx context.Context, api *tg.Client, panel *Panel, panelClient *botconf.Client, session panelSession, helperReady <-chan *gotd.Client) {
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
// through: it answers the panel's inline queries and its button presses.
// Once logged in it hands itself to the main session over ready.
func (c Connector) runHelper(ctx context.Context, panel *Panel, ready chan<- *gotd.Client) error {
	dispatcher := tg.NewUpdateDispatcher()
	var helper *gotd.Client
	dispatcher.OnBotInlineQuery(func(ctx context.Context, _ tg.Entities, update *tg.UpdateBotInlineQuery) error {
		if taken, err := panel.HandleInlineQuery(ctx, update); taken {
			return err
		}
		// Anything else typed after @<helper bot> gets nothing.
		_, err := helper.API().MessagesSetInlineBotResults(ctx, &tg.MessagesSetInlineBotResultsRequest{
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
		_, err := helper.API().MessagesSetBotCallbackAnswer(ctx, &tg.MessagesSetBotCallbackAnswerRequest{QueryID: update.QueryID})
		return err
	})
	helper = gotd.NewClient(c.credentials.AppID, c.credentials.AppHash, gotd.Options{
		SessionStorage: &session.FileStorage{Path: c.cfg.Panel.SessionFile},
		UpdateHandler:  dispatcher,
	})
	return helper.Run(ctx, func(ctx context.Context) error {
		if err := authenticateBot(ctx, helper, c.credentials.PanelBotToken); err != nil {
			return fmt.Errorf("settings panel helper bot: %w", err)
		}
		c.logger.Info().Msg("Telegram panel helper bot connected")
		ready <- helper
		<-ctx.Done()
		return ctx.Err()
	})
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
