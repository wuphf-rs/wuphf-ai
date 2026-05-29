package core

import (
	"context"
	"errors"
	"fmt"
	"log"
	"time"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

const (
	initialBackoff  = time.Second
	maxBackoff      = 30 * time.Second
	maxSendAttempts = 3
	sendRetryDelay  = 500 * time.Millisecond
)

type Sender interface {
	SendMessage(context.Context, *wuphfpb.MessageCreateDispatch) (string, error)
}

type Client struct {
	cfg config.File
}

func New(cfg config.File) Client {
	return Client{cfg: cfg}
}

// Run maintains a connector session against the core, reconnecting with
// exponential backoff when a session ends for any reason other than context
// cancellation. Unacknowledged dispatches are redelivered by the core on
// reconnect, so a dropped session does not lose work.
func (c Client) Run(ctx context.Context, sender Sender, inputs <-chan *wuphfpb.ClientFrame) error {
	backoff := initialBackoff
	for {
		err := c.runSession(ctx, sender, inputs, func() { backoff = initialBackoff })
		if ctx.Err() != nil {
			return ctx.Err()
		}
		log.Printf("connector session ended, reconnecting in %s: %v", backoff, err)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(backoff):
		}
		if backoff *= 2; backoff > maxBackoff {
			backoff = maxBackoff
		}
	}
}

func (c Client) runSession(
	ctx context.Context,
	sender Sender,
	inputs <-chan *wuphfpb.ClientFrame,
	onConnected func(),
) error {
	connection, err := grpc.NewClient(c.cfg.CoreAddress, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return fmt.Errorf("connect to core: %w", err)
	}
	defer connection.Close()
	stream, err := wuphfpb.NewConnectorTransportClient(connection).OpenSession(ctx)
	if err != nil {
		return fmt.Errorf("open connector session: %w", err)
	}
	if err := stream.Send(&wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_Registration{Registration: &wuphfpb.Registration{
			ConnectorId: c.cfg.ConnectorID,
			Platform:    wuphfpb.Platform_PLATFORM_WHATSAPP,
			EndpointIds: c.cfg.Endpoints,
		}},
	}); err != nil {
		return fmt.Errorf("register connector: %w", err)
	}
	onConnected()
	frames := make(chan *wuphfpb.ServerFrame)
	receiveErrors := make(chan error, 1)
	go func() {
		for {
			frame, err := stream.Recv()
			if err != nil {
				receiveErrors <- err
				return
			}
			select {
			case frames <- frame:
			case <-ctx.Done():
				return
			}
		}
	}()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case err := <-receiveErrors:
			return fmt.Errorf("receive core dispatch: %w", err)
		case input := <-inputs:
			if err := stream.Send(input); err != nil {
				return fmt.Errorf("send WhatsApp input: %w", err)
			}
		case frame := <-frames:
			switch body := frame.Body.(type) {
			case *wuphfpb.ServerFrame_MessageCreate:
				messageID, err := sendWithRetry(ctx, sender, body.MessageCreate)
				if err != nil {
					return fmt.Errorf("send dispatch %s: %w", body.MessageCreate.DispatchId, err)
				}
				if err := stream.Send(&wuphfpb.ClientFrame{
					Body: &wuphfpb.ClientFrame_Acknowledgement{Acknowledgement: &wuphfpb.Acknowledgement{
						DispatchId:        body.MessageCreate.DispatchId,
						PlatformMessageId: messageID,
					}},
				}); err != nil {
					return fmt.Errorf("acknowledge dispatch %s: %w", body.MessageCreate.DispatchId, err)
				}
			case *wuphfpb.ServerFrame_Rejected:
				return fmt.Errorf("core rejected connector operation: %s", body.Rejected.Reason)
			default:
				return errors.New("core returned empty connector frame")
			}
		}
	}
}

// sendWithRetry retries a transient dispatch send a bounded number of times
// before surfacing the failure to the session loop, which then drops the
// session so the core can redeliver the still-unacknowledged dispatch.
func sendWithRetry(ctx context.Context, sender Sender, dispatch *wuphfpb.MessageCreateDispatch) (string, error) {
	var lastErr error
	for attempt := 0; attempt < maxSendAttempts; attempt++ {
		if attempt > 0 {
			select {
			case <-ctx.Done():
				return "", ctx.Err()
			case <-time.After(sendRetryDelay):
			}
		}
		messageID, err := sender.SendMessage(ctx, dispatch)
		if err == nil {
			return messageID, nil
		}
		lastErr = err
		log.Printf("dispatch %s send attempt %d failed: %v", dispatch.DispatchId, attempt+1, err)
	}
	return "", lastErr
}
