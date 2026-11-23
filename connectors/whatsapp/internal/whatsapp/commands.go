package whatsappconnector

import (
	"context"
	"errors"
	"sync/atomic"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/appstate"
)

// The commands the owner can run on this connector from the core's settings
// panel (plan 058): a resync of every app-state collection, and a link code
// now rather than at the next gap. Both answer at once; the work goes on in
// the background.
const (
	commandResyncAppState = "resync_app_state"
	commandFreshLinkCode  = "fresh_link_code"
)

// appStateFetcher is the part of the WhatsApp client a resync uses.
type appStateFetcher interface {
	FetchAppState(ctx context.Context, name appstate.WAPatchName, fullSync, onlyIfNotSynced bool) error
}

// linkedClient is the WhatsApp client of the session that is linked, nil while
// none is.
type linkedClient struct {
	current atomic.Pointer[whatsmeow.Client]
}

func (l *linkedClient) set(client *whatsmeow.Client) {
	l.current.Store(client)
}

func (l *linkedClient) fetcher() appStateFetcher {
	if client := l.current.Load(); client != nil {
		return client
	}
	return nil
}

// whatsappCommands runs the connector's commands (coreclient.Commands).
type whatsappCommands struct {
	// client is the linked session's client, or nil.
	client    func() appStateFetcher
	linking   *linking
	resyncing atomic.Bool
	logger    zerolog.Logger
	// background is the context resyncs run in, outliving the command.
	background context.Context
}

func newCommands(background context.Context, client func() appStateFetcher, linking *linking, logger zerolog.Logger) *whatsappCommands {
	return &whatsappCommands{
		client:     client,
		linking:    linking,
		logger:     logger.With().Str("component", "commands").Logger(),
		background: background,
	}
}

func (c *whatsappCommands) Declarations() []*wuphfpb.ConnectorCommand {
	return []*wuphfpb.ConnectorCommand{
		{Id: commandResyncAppState, Title: "🔄 Resync app state"},
		{Id: commandFreshLinkCode, Title: "🔗 Send a new link code"},
	}
}

func (c *whatsappCommands) Run(_ context.Context, id string) (string, error) {
	switch id {
	case commandResyncAppState:
		return c.resyncAppState()
	case commandFreshLinkCode:
		return c.freshLinkCode(), nil
	default:
		return "", errors.New("unknown command " + id)
	}
}

// resyncAppState fetches every collection whole again, one at a time, in the
// background; one requested resync runs at a time.
func (c *whatsappCommands) resyncAppState() (string, error) {
	client := c.client()
	if client == nil {
		return "", errors.New("WhatsApp is not linked; link it first")
	}
	if !c.resyncing.CompareAndSwap(false, true) {
		return "", errors.New("a resync is already running")
	}
	go func() {
		defer c.resyncing.Store(false)
		for _, name := range appstate.AllPatchNames {
			if err := client.FetchAppState(c.background, name, true, false); err != nil {
				c.logger.Warn().Err(err).Str("collection", string(name)).Msg("could not resync a WhatsApp app-state collection")
			}
		}
		c.logger.Info().Msg("resynced every WhatsApp app-state collection on request")
	}()
	return "Resyncing contacts, pins, mutes and archives; changes arrive as they sync", nil
}

func (c *whatsappCommands) freshLinkCode() string {
	if c.client() != nil {
		return "WhatsApp is already linked"
	}
	c.linking.FreshCode()
	return "A new link code is on its way (within a minute)"
}
