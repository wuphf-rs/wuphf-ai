package telegramconnector

import (
	"context"
	"errors"
	"strings"
	"sync"

	"github.com/gotd/td/tg"
	render "github.com/propheci/botconf/go/render/telegram"
	botconf "github.com/propheci/botconf/go/settings"
	"github.com/rs/zerolog"
)

// HelperChats serves the settings panel in the helper bot's own private
// chats. A user session's panel only obeys that account, so an owner who
// bridges through another account opens the panel by messaging the helper
// bot instead: there it is an ordinary bot panel with an inline keyboard, and
// the core checks the sender against the connector's owner_ids like any
// other panel actor.
//
// Only private chats count. The helper bot is not the bridge account: nothing
// it receives is mirrored, and groups it happens to be in get no answer.
type HelperChats struct {
	command string
	logger  zerolog.Logger

	mu     sync.RWMutex
	client *botconf.Client
	panel  *render.BotPanel
	names  map[string]bool
	// hashes are the access hashes of the users who wrote to the helper, from
	// their messages' entities, so replies and redraws address them properly.
	hashes map[int64]int64
}

// NewHelperChats takes the panel command (without the slash).
func NewHelperChats(command string, logger zerolog.Logger) *HelperChats {
	return &HelperChats{
		command: command,
		logger:  logger.With().Str("component", "helper-panel").Logger(),
		hashes:  map[int64]int64{},
	}
}

// start readies the panel once the helper bot has logged in: client is the
// core's settings service, api the helper's, and username the helper's, which
// a command may be addressed to (/wuphf@<username>). Until then messages and
// presses are not taken.
func (h *HelperChats) start(client *botconf.Client, api *tg.Client, username string) {
	panel := render.NewBotPanel(client, api, render.WithPeerResolver(h.resolvePeer))
	h.mu.Lock()
	defer h.mu.Unlock()
	h.client, h.panel = client, panel
	h.names = map[string]bool{}
	if username != "" {
		h.names[strings.ToLower(username)] = true
	}
}

func (h *HelperChats) state() (*botconf.Client, *render.BotPanel, map[string]bool) {
	h.mu.RLock()
	defer h.mu.RUnlock()
	return h.client, h.panel, h.names
}

// HandleMessage takes a private message to the helper bot: the panel command,
// or an answer to a question the panel asked. It returns false for anything
// else, which the helper then ignores.
func (h *HelperChats) HandleMessage(ctx context.Context, entities tg.Entities, class tg.MessageClass) (bool, error) {
	message, ok := class.(*tg.Message)
	if !ok || message.Out {
		return false, nil
	}
	chat, private := message.PeerID.(*tg.PeerUser)
	if !private {
		return false, nil
	}
	client, panel, names := h.state()
	if panel == nil {
		return false, nil
	}
	h.remember(entities, chat.UserID)
	peer, err := h.resolvePeer(chat)
	if err != nil {
		return false, err
	}
	ctx, cancel := context.WithTimeout(ctx, panelCallTimeout)
	defer cancel()

	args, isCommand := matchCommand(message.Message, h.command, names)
	if !isCommand {
		return panel.HandleMessage(ctx, peer, message)
	}
	// Ask the core first, so someone who is not an owner gets no reply at
	// all rather than an error message from the bot.
	if err := probe(ctx, client, chat.UserID); err != nil {
		if errors.Is(err, botconf.ErrPermissionDenied) {
			h.logger.Info().Int64("user", chat.UserID).Msg("panel command to the helper bot from someone not in owner_ids; ignored")
			return true, nil
		}
		return true, err
	}
	return true, panel.Command(ctx, peer, chat.UserID, message.ID, args)
}

// HandleCallback takes a press of a button on a panel in the helper bot's
// private chats. It returns false for presses it does not own, which the
// helper answers silently.
func (h *HelperChats) HandleCallback(ctx context.Context, entities tg.Entities, update *tg.UpdateBotCallbackQuery) (bool, error) {
	chat, private := update.Peer.(*tg.PeerUser)
	if !private {
		return false, nil
	}
	_, panel, _ := h.state()
	if panel == nil {
		return false, nil
	}
	h.remember(entities, chat.UserID)
	ctx, cancel := context.WithTimeout(ctx, panelCallTimeout)
	defer cancel()
	return panel.HandleCallback(ctx, update)
}

// remember records user's access hash when the update's entities carry it.
func (h *HelperChats) remember(entities tg.Entities, user int64) {
	found, ok := entities.Users[user]
	if !ok || found.AccessHash == 0 {
		return
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	h.hashes[user] = found.AccessHash
}

// resolvePeer addresses a private chat with the access hash its user's
// messages carried, else a zero hash, which Telegram accepts from a bot for a
// user who has written to it.
func (h *HelperChats) resolvePeer(peer tg.PeerClass) (tg.InputPeerClass, error) {
	user, ok := peer.(*tg.PeerUser)
	if !ok {
		return render.ZeroHashPeer(peer)
	}
	h.mu.RLock()
	defer h.mu.RUnlock()
	return &tg.InputPeerUser{UserID: user.UserID, AccessHash: h.hashes[user.UserID]}, nil
}

// helperCommands advertises the panel command in the helper bot's command
// menu for private chats.
func helperCommands(command string) *tg.BotsSetBotCommandsRequest {
	return &tg.BotsSetBotCommandsRequest{
		Scope:    &tg.BotCommandScopeUsers{},
		LangCode: "",
		Commands: []tg.BotCommand{{Command: command, Description: "Open the wuphf settings"}},
	}
}
