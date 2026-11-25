package telegramconnector

import (
	"context"
	"errors"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/gotd/td/tg"
	render "github.com/propheci/botconf/go/render/telegram"
	botconf "github.com/propheci/botconf/go/settings"
	"github.com/rs/zerolog"
)

// panelCallTimeout bounds one trip to the core's settings service, so a slow
// core cannot hold up the update stream for long.
const panelCallTimeout = 30 * time.Second

// Panel opens the core's settings panel from Telegram. The connector stays a
// thin front end: it recognizes the panel command, says who acts, and lets
// botconf's renderer draw what the core returns and forward presses and typed
// answers. The core decides who may do what.
//
// A bot session draws the panel with an inline keyboard. A user session
// cannot attach buttons to its messages, so it goes through a helper bot in
// inline mode, or falls back to text when there is none.
//
// Panel implements PanelRouter. Until the session has started it, it only
// takes the command, so that it is never mirrored, and does nothing else.
type Panel struct {
	command string
	peers   *PeerCache
	logger  zerolog.Logger

	mu    sync.RWMutex
	front panelFront
	self  int64
	names map[string]bool
}

// panelFront is how a session kind shows the panel.
type panelFront interface {
	// open runs the panel command for actor in peer: args is the text after
	// the command, replyTo the forum topic to answer in (0: none).
	open(ctx context.Context, peer tg.InputPeerClass, actor int64, replyTo int, args string) error
	// message takes msg as an answer to a pending question.
	message(ctx context.Context, peer tg.InputPeerClass, msg *tg.Message) (bool, error)
	// api is the session's API, to delete the command once handled.
	api() *tg.Client
}

// NewPanel takes the panel command (without the slash) and resolves chats
// through peers.
func NewPanel(command string, peers *PeerCache, logger zerolog.Logger) *Panel {
	return &Panel{
		command: command,
		peers:   peers,
		logger:  logger.With().Str("component", "panel").Logger(),
	}
}

// panelSession is what Start needs to know about the logged-in account.
type panelSession struct {
	// self is the session's own user id: the actor of its own messages.
	self int64
	// names are the usernames a command may be addressed to
	// (/wuphf@<name>), without the @.
	names []string
}

// startBot draws the panel through a bot session.
func (p *Panel) startBot(client *botconf.Client, api *tg.Client, session panelSession) {
	p.start(&botFront{
		client: client,
		tgAPI:  api,
		panel:  render.NewBotPanel(client, api, render.WithContext(panelContext)),
	}, session)
}

// startUser draws the panel through a user session; inline is the renderer,
// with or without a helper bot.
func (p *Panel) startUser(client *botconf.Client, api *tg.Client, inline *render.InlinePanel, session panelSession) {
	p.start(&userFront{client: client, tgAPI: api, inline: inline, self: session.self}, session)
}

func (p *Panel) start(front panelFront, session panelSession) {
	names := map[string]bool{}
	for _, name := range session.names {
		if name != "" {
			names[strings.ToLower(name)] = true
		}
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	p.front, p.self = front, session.self
	for name := range p.names {
		names[name] = true
	}
	p.names = names
}

// addNames lets commands be addressed to more usernames (e.g. the helper
// bot's once it is known).
func (p *Panel) addNames(names ...string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.names == nil {
		p.names = map[string]bool{}
	}
	for _, name := range names {
		if name != "" {
			p.names[strings.ToLower(name)] = true
		}
	}
}

func (p *Panel) state() (panelFront, int64, map[string]bool) {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.front, p.self, p.names
}

// Take implements PanelRouter: it takes the panel command, whoever sends it,
// and answers to the panel's pending questions.
func (p *Panel) Take(ctx context.Context, _ tg.Entities, message *tg.Message) bool {
	front, self, names := p.state()
	args, isCommand := matchCommand(message.Message, p.command, names)
	if !isCommand && front == nil {
		return false
	}
	ctx, cancel := context.WithTimeout(ctx, panelCallTimeout)
	defer cancel()

	peer, err := p.peers.InputPeer(ctx, formatPeer(message.PeerID))
	if err != nil {
		if isCommand {
			p.logger.Warn().Err(err).Msg("cannot address the chat of a panel command")
		}
		return isCommand
	}
	if !isCommand {
		taken, err := front.message(ctx, peer, message)
		if err != nil {
			p.logger.Warn().Err(err).Msg("settings panel answer failed")
		}
		return taken
	}

	actor, ok := panelActor(message, self)
	if front == nil || !ok {
		// Not ready yet, or no user to act for (e.g. a channel post): the
		// command is still never mirrored.
		return true
	}
	replyTo, _ := topicRootID(message)
	if err := front.open(ctx, peer, actor, replyTo, args); err != nil {
		if errors.Is(err, botconf.ErrPermissionDenied) {
			p.logger.Debug().Int64("actor", actor).Msg("panel command from someone not allowed; ignored")
			return true
		}
		p.logger.Warn().Err(err).Msg("settings panel command failed")
		return true
	}
	deleteMessages(ctx, front.api(), peer, message.ID)
	return true
}

// HandleCallback handles a press of a bot session's panel button. It returns
// false when the press is not the panel's.
func (p *Panel) HandleCallback(ctx context.Context, update *tg.UpdateBotCallbackQuery) (bool, error) {
	front, _, _ := p.state()
	bot, ok := front.(*botFront)
	if !ok {
		return false, nil
	}
	ctx, cancel := context.WithTimeout(ctx, panelCallTimeout)
	defer cancel()
	return bot.panel.HandleCallback(ctx, update)
}

// HandleInlineQuery answers the helper bot's inline query for a user
// session's panel. It returns false when the query is not a panel's.
func (p *Panel) HandleInlineQuery(ctx context.Context, update *tg.UpdateBotInlineQuery) (bool, error) {
	front, _, _ := p.state()
	user, ok := front.(*userFront)
	if !ok {
		return false, nil
	}
	return user.inline.HandleInlineQuery(ctx, update)
}

// HandleInlineCallback handles a press of a user session's panel, which
// reaches the helper bot. It returns false when the press is not the panel's.
func (p *Panel) HandleInlineCallback(ctx context.Context, update *tg.UpdateInlineBotCallbackQuery) (bool, error) {
	front, _, _ := p.state()
	user, ok := front.(*userFront)
	if !ok {
		return false, nil
	}
	ctx, cancel := context.WithTimeout(ctx, panelCallTimeout)
	defer cancel()
	return user.inline.HandleInlineCallback(ctx, update)
}

// matchCommand says whether text is the panel command, `/<command>` or
// `/<command>@<name>` as its first word (the command case sensitive, like
// Telegram's), and returns what follows it.
func matchCommand(text, command string, names map[string]bool) (string, bool) {
	if !strings.HasPrefix(text, "/") {
		return "", false
	}
	word, rest, _ := strings.Cut(text, " ")
	if newline := strings.IndexAny(word, "\n\t"); newline >= 0 {
		rest = word[newline+1:] + " " + rest
		word = word[:newline]
	}
	name, addressee, addressed := strings.Cut(strings.TrimPrefix(word, "/"), "@")
	if name != command {
		return "", false
	}
	if addressed && !names[strings.ToLower(addressee)] {
		return "", false
	}
	return strings.TrimSpace(rest), true
}

// panelActor is the Telegram user a panel message acts for: its sender, the
// session's own account for its outgoing messages, or the other side of a
// private chat.
func panelActor(message *tg.Message, self int64) (int64, bool) {
	if from, ok := message.FromID.(*tg.PeerUser); ok {
		return from.UserID, true
	}
	if message.Out && self != 0 {
		return self, true
	}
	if peer, ok := message.PeerID.(*tg.PeerUser); ok && !message.Out {
		return peer.UserID, true
	}
	return 0, false
}

// panelActorFor is the settings actor for a Telegram user, as botconf's
// renderer sends it: the core checks the id against the connector's owner_ids.
func panelActorFor(user int64) *botconf.Actor {
	return &botconf.Actor{Domain: render.UserDomain, ID: strconv.FormatInt(user, 10)}
}

// probe asks the core for the panel's home screen as actor, which fails with
// botconf.ErrPermissionDenied for someone not allowed. A command from them
// then gets no reply at all.
func probe(ctx context.Context, client *botconf.Client, actor int64) error {
	_, err := client.Render(ctx, panelActorFor(actor), botconf.Home(), botconf.View{Limits: render.Limits(render.DefaultPrefix)})
	return err
}

// botFront draws the panel through a bot session.
type botFront struct {
	client *botconf.Client
	tgAPI  *tg.Client
	panel  *render.BotPanel
}

func (f *botFront) api() *tg.Client { return f.tgAPI }

func (f *botFront) open(ctx context.Context, peer tg.InputPeerClass, actor int64, replyTo int, args string) error {
	if err := probe(ctx, f.client, actor); err != nil {
		return err
	}
	return f.panel.Command(ctx, peer, actor, replyTo, args)
}

func (f *botFront) message(ctx context.Context, peer tg.InputPeerClass, msg *tg.Message) (bool, error) {
	return f.panel.HandleMessage(ctx, peer, msg)
}

// userFront draws the panel through a user session, which only its own
// account drives.
type userFront struct {
	client *botconf.Client
	tgAPI  *tg.Client
	inline *render.InlinePanel
	self   int64
}

func (f *userFront) api() *tg.Client { return f.tgAPI }

func (f *userFront) open(ctx context.Context, peer tg.InputPeerClass, actor int64, replyTo int, args string) error {
	if actor != f.self {
		// The account acts as itself; nobody else may drive its panel.
		return botconf.ErrPermissionDenied
	}
	if err := probe(ctx, f.client, actor); err != nil {
		return err
	}
	return f.inline.Command(ctx, peer, replyTo, args)
}

func (f *userFront) message(ctx context.Context, peer tg.InputPeerClass, msg *tg.Message) (bool, error) {
	return f.inline.HandleMessage(ctx, peer, msg)
}

// deleteMessages removes messages in peer, best effort: an account cannot
// always delete someone else's message.
func deleteMessages(ctx context.Context, api *tg.Client, peer tg.InputPeerClass, ids ...int) {
	if api == nil || len(ids) == 0 {
		return
	}
	if channel, ok := peer.(*tg.InputPeerChannel); ok {
		_, _ = api.ChannelsDeleteMessages(ctx, &tg.ChannelsDeleteMessagesRequest{
			Channel: &tg.InputChannel{ChannelID: channel.ChannelID, AccessHash: channel.AccessHash},
			ID:      ids,
		})
		return
	}
	_, _ = api.MessagesDeleteMessages(ctx, &tg.MessagesDeleteMessagesRequest{Revoke: true, ID: ids})
}
