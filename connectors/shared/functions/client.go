// Package functions is the Go SDK for the core's remote functions. A Client
// calls functions through the core's FunctionTransport; Serve registers a
// provider's functions over ProviderTransport and answers the calls the core
// brokers to it. The contracts of specific functions (media.convert@1) live
// here too, so callers and providers share one definition of their bodies.
package functions

import (
	"context"
	"encoding/json"
	"fmt"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
)

// Client calls remote functions through the core's FunctionTransport.
type Client struct {
	connection *grpc.ClientConn
	client     wuphfpb.FunctionTransportClient
	token      string
	caller     string
}

// Dial connects to the core. token may be empty (core without an invoke
// token); caller labels this process in the core's logs (a connector id).
func Dial(address, token, caller string) (*Client, error) {
	return dial(address, token, caller)
}

func dial(address, token, caller string, extra ...grpc.DialOption) (*Client, error) {
	options := append([]grpc.DialOption{grpc.WithTransportCredentials(insecure.NewCredentials())}, extra...)
	connection, err := grpc.NewClient(address, options...)
	if err != nil {
		return nil, fmt.Errorf("connect to core function service: %w", err)
	}
	return &Client{
		connection: connection,
		client:     wuphfpb.NewFunctionTransportClient(connection),
		token:      token,
		caller:     caller,
	}, nil
}

func (c *Client) Close() error {
	return c.connection.Close()
}

// Invoke calls name@major. args is marshalled to JSON; the JSON result is
// decoded into result unless result is nil. It returns the result media ids.
// The call's deadline is ctx's deadline.
func (c *Client) Invoke(ctx context.Context, name string, major uint32, args any, mediaIDs []string, result any) ([]string, error) {
	body, err := json.Marshal(args)
	if err != nil {
		return nil, fmt.Errorf("invoke %s@%d: encode args: %w", name, major, err)
	}
	if c.token != "" {
		ctx = metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+c.token)
	}
	response, err := c.client.Invoke(ctx, &wuphfpb.InvokeRequest{
		Function: &wuphfpb.FunctionRef{Name: name, Major: major},
		Args:     body,
		MediaIds: mediaIDs,
		Caller:   c.caller,
	})
	if err != nil {
		return nil, fmt.Errorf("invoke %s@%d: %w", name, major, err)
	}
	if result != nil {
		if err := json.Unmarshal(response.GetResult(), result); err != nil {
			return nil, fmt.Errorf("invoke %s@%d: decode result from provider %s: %w", name, major, response.GetProviderId(), err)
		}
	}
	return response.GetMediaIds(), nil
}
