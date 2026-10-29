package settings

import (
	"context"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	botconf "github.com/propheci/botconf/go/settings"
	"github.com/rs/zerolog"
)

// Connection is a connector's runtime settings: the declaration to send when
// registering, the live value, and the core's settings service it follows.
type Connection[T any] struct {
	// Declaration goes into the registration (coreclient.Options.Settings).
	Declaration *wuphfpb.ConnectorSettingsDeclaration
	// Live is the current value; read it once per use.
	Live *Live[T]

	base        T
	connectorID string
	client      *botconf.Client
	logger      zerolog.Logger
}

// Connect declares base (the connector's own file values, defaults applied)
// and dials the core's settings service with the connector's core token. Call
// Follow to apply the core's pushes, and Close when done.
func Connect[T any](coreAddress, coreToken, connectorID string, base T, logger zerolog.Logger) (*Connection[T], error) {
	declaration, err := Declare(base)
	if err != nil {
		return nil, err
	}
	logger = logger.With().Str("component", "settings").Logger()
	client, err := botconf.Dial(coreAddress,
		botconf.WithBearer(coreToken),
		// The stream reconnects by itself (e.g. while the core restarts).
		botconf.WithWatchErrors(func(err error) {
			logger.Debug().Err(err).Msg("settings stream interrupted; reconnecting")
		}),
	)
	if err != nil {
		return nil, err
	}
	return &Connection[T]{
		Declaration: declaration,
		Live:        NewLive(base),
		base:        base,
		connectorID: connectorID,
		client:      client,
		logger:      logger,
	}, nil
}

// Follow applies the core's pushes until ctx ends (see Watch); onChange runs
// after each one. A push that does not decode is logged and skipped: the
// connector keeps its last value.
func (c *Connection[T]) Follow(ctx context.Context, onChange func(old, new T)) {
	Watch(ctx, c.client, c.connectorID, c.base, c.Live, onChange, func(err error) {
		c.logger.Warn().Err(err).Msg("ignoring a runtime settings update")
	})
}

// Close closes the connection to the settings service.
func (c *Connection[T]) Close() error {
	return c.client.Close()
}
