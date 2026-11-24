package settings

import (
	"context"
	"fmt"

	botconf "github.com/propheci/botconf/go/settings"
	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/grpc"
)

// Connection is a connector's or a plugin's runtime settings: the
// declaration, the live value, and the core's settings service it follows.
type Connection[T any] struct {
	// Declaration goes into a connector's registration
	// (coreclient.Options.Settings); ConnectPlugin has already sent it.
	Declaration *wuphfpb.ConnectorSettingsDeclaration
	// Live is the current value; read it once per use.
	Live *Live[T]

	base   T
	prefix string
	client *botconf.Client
	logger zerolog.Logger
}

// Connect declares base (the connector's own file values, defaults applied)
// and dials the core's settings service with the connector's core token. Call
// Follow to apply the core's pushes, and Close when done.
func Connect[T any](coreAddress, coreToken, connectorID string, base T, logger zerolog.Logger) (*Connection[T], error) {
	declaration, err := Declare(base)
	if err != nil {
		return nil, err
	}
	return dial(coreAddress, coreToken, Prefix(connectorID), declaration, base, logger)
}

// Declarer is the core's plugin service (wuphfpb.PluginTransportClient
// satisfies it), reached with the observer token.
type Declarer interface {
	DeclareSettings(ctx context.Context, in *wuphfpb.DeclarePluginSettingsRequest, opts ...grpc.CallOption) (*wuphfpb.DeclarePluginSettingsResponse, error)
}

// ConnectPlugin declares base (the plugin's own file values, defaults
// applied) as plugin pluginID's settings through declarer, then dials the
// core's settings service with the observer token. Call Follow to apply the
// core's pushes, and Close when done. A refused declaration is an error: the
// plugin then has no section to follow.
func ConnectPlugin[T any](
	ctx context.Context,
	declarer Declarer,
	coreAddress, observerToken, pluginID string,
	base T,
	logger zerolog.Logger,
) (*Connection[T], error) {
	declaration, err := Declare(base)
	if err != nil {
		return nil, err
	}
	if _, err := declarer.DeclareSettings(ctx, &wuphfpb.DeclarePluginSettingsRequest{
		PluginId: pluginID,
		Settings: declaration,
	}); err != nil {
		return nil, fmt.Errorf("declare the settings to the core: %w", err)
	}
	return dial(coreAddress, observerToken, PluginPrefix(pluginID), declaration, base, logger)
}

func dial[T any](
	coreAddress, token, prefix string,
	declaration *wuphfpb.ConnectorSettingsDeclaration,
	base T,
	logger zerolog.Logger,
) (*Connection[T], error) {
	logger = logger.With().Str("component", "settings").Logger()
	client, err := botconf.Dial(coreAddress,
		botconf.WithBearer(token),
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
		prefix:      prefix,
		client:      client,
		logger:      logger,
	}, nil
}

// Follow applies the core's pushes until ctx ends (see WatchSection);
// onChange runs after each one. A push that does not decode is logged and
// skipped: the last value is kept.
func (c *Connection[T]) Follow(ctx context.Context, onChange func(old, new T)) {
	WatchSection(ctx, c.client, c.prefix, c.base, c.Live, onChange, func(err error) {
		c.logger.Warn().Err(err).Msg("ignoring a runtime settings update")
	})
}

// Close closes the connection to the settings service.
func (c *Connection[T]) Close() error {
	return c.client.Close()
}
