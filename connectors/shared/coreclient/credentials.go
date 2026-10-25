package coreclient

import (
	"context"
	"fmt"
	"os"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
)

// bearerCredentials attaches `authorization: Bearer <token>` to every call on
// a connection, which is how the core authenticates connectors and media
// clients.
type bearerCredentials struct{ token string }

var _ credentials.PerRPCCredentials = bearerCredentials{}

func (b bearerCredentials) GetRequestMetadata(context.Context, ...string) (map[string]string, error) {
	return map[string]string{"authorization": "Bearer " + b.token}, nil
}

// RequireTransportSecurity is false because every core connection is plain
// gRPC today; the token is what the core checks.
func (bearerCredentials) RequireTransportSecurity() bool { return false }

// WithToken makes every call on the connection present token to the core.
func WithToken(token string) grpc.DialOption {
	return grpc.WithPerRPCCredentials(bearerCredentials{token: token})
}

// TokenFromEnv reads a connector's core token from the environment variable
// its config names in core_token_env. Tokens are only ever read from the
// environment, so they stay out of config files and the process list.
func TokenFromEnv(variable string) (string, error) {
	token := os.Getenv(variable)
	if token == "" {
		return "", fmt.Errorf("environment variable %s (core_token_env) is unset or empty", variable)
	}
	return token, nil
}
