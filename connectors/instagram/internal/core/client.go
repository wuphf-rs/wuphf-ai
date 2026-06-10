package core

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/akshettrj/wuphf/connectors/instagram/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

const (
	initialBackoff  = time.Second
	maxBackoff      = 30 * time.Second
	maxSendAttempts = 3
	sendRetryDelay  = 500 * time.Millisecond
)

// PermanentError marks a send failure that can never succeed on retry (e.g. an
// invalid target). The session loop reports it to the core for dead-lettering
// instead of dropping the session, which would only redeliver the same dispatch.
type PermanentError struct{ Err error }

func (e PermanentError) Error() string { return e.Err.Error() }
func (e PermanentError) Unwrap() error { return e.Err }

// Permanent wraps err so the session loop dead-letters the dispatch.
func Permanent(err error) error {
	if err == nil {
		return nil
	}
	return PermanentError{Err: err}
}

func isPermanent(err error) bool {
	var marked PermanentError
	return errors.As(err, &marked)
}

type Sender interface {
	// SendMessage delivers a dispatch and returns the resulting platform message
	// id. threadID is non-empty only when the send provisioned a new thread, so
	// the core can persist the conversation-to-thread mapping.
	SendMessage(context.Context, *wuphfpb.MessageCreateDispatch) (messageID string, threadID string, err error)
	// EditMessage revises an already-delivered copy and returns its platform
	// message id, which an edit leaves unchanged.
	EditMessage(context.Context, *wuphfpb.MessageEditDispatch) (messageID string, err error)
	// DeleteMessage revokes an already-delivered copy and returns its platform
	// message id, which a delete leaves unchanged.
	DeleteMessage(context.Context, *wuphfpb.MessageDeleteDispatch) (messageID string, err error)
}

// sendOp performs one outbound delivery attempt, returning the resulting
// platform message id and, when a thread was provisioned, its id.
type sendOp func(context.Context) (messageID string, threadID string, err error)

type Client struct {
	cfg    config.File
	logger zerolog.Logger
}

func New(cfg config.File, logger zerolog.Logger) Client {
	return Client{cfg: cfg, logger: logger.With().Str("component", "core-client").Logger()}
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
		c.logger.Warn().Err(err).Dur("backoff", backoff).Msg("connector session ended, reconnecting")
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
			Platform:    wuphfpb.Platform_PLATFORM_INSTAGRAM,
			Capabilities: &wuphfpb.Capabilities{
				SupportsThreads:     c.cfg.SupportsThreads,
				SupportsTopicCreate: c.cfg.SupportsTopicCreate,
			},
		}},
	}); err != nil {
		return fmt.Errorf("register connector: %w", err)
	}
	onConnected()
	c.logger.Info().Str("connector_id", c.cfg.ConnectorID).Msg("connector session registered with core")
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
				return fmt.Errorf("send Instagram input: %w", err)
			}
		case frame := <-frames:
			switch body := frame.Body.(type) {
			case *wuphfpb.ServerFrame_MessageCreate:
				dispatch := body.MessageCreate
				if err := c.deliver(ctx, stream, dispatch.DispatchId, func(ctx context.Context) (string, string, error) {
					return sender.SendMessage(ctx, dispatch)
				}); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_MessageEdit:
				dispatch := body.MessageEdit
				if err := c.deliver(ctx, stream, dispatch.DispatchId, func(ctx context.Context) (string, string, error) {
					messageID, err := sender.EditMessage(ctx, dispatch)
					return messageID, "", err
				}); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_MessageDelete:
				dispatch := body.MessageDelete
				if err := c.deliver(ctx, stream, dispatch.DispatchId, func(ctx context.Context) (string, string, error) {
					messageID, err := sender.DeleteMessage(ctx, dispatch)
					return messageID, "", err
				}); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_Rejected:
				return fmt.Errorf("core rejected connector operation: %s", body.Rejected.Reason)
			default:
				return errors.New("core returned empty connector frame")
			}
		}
	}
}

// deliver runs op with bounded retry, then resolves the dispatch with the core:
// a successful send is acknowledged, a permanent failure is dead-lettered, and a
// transient failure is surfaced to the session loop, which drops the session so
// the core can redeliver the still-unacknowledged dispatch.
func (c Client) deliver(ctx context.Context, stream wuphfpb.ConnectorTransport_OpenSessionClient, dispatchID string, op sendOp) error {
	messageID, threadID, err := c.sendWithRetry(ctx, dispatchID, op)
	if err != nil {
		if !isPermanent(err) {
			return fmt.Errorf("send dispatch %s: %w", dispatchID, err)
		}
		// A permanent failure cannot be retried; report it so the core
		// dead-letters the dispatch and the session keeps serving others.
		c.logger.Error().Err(err).Str("dispatch_id", dispatchID).Msg("dispatch permanently failed, dead-lettering")
		if err := stream.Send(&wuphfpb.ClientFrame{
			Body: &wuphfpb.ClientFrame_DispatchFailed{DispatchFailed: &wuphfpb.DispatchFailed{
				DispatchId: dispatchID,
				Reason:     err.Error(),
			}},
		}); err != nil {
			return fmt.Errorf("report failed dispatch %s: %w", dispatchID, err)
		}
		return nil
	}
	acknowledgement := &wuphfpb.Acknowledgement{
		DispatchId:        dispatchID,
		PlatformMessageId: messageID,
	}
	if threadID != "" {
		acknowledgement.ThreadId = &threadID
	}
	if err := stream.Send(&wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_Acknowledgement{Acknowledgement: acknowledgement},
	}); err != nil {
		return fmt.Errorf("acknowledge dispatch %s: %w", dispatchID, err)
	}
	c.logger.Debug().Str("dispatch_id", dispatchID).Str("platform_message_id", messageID).Msg("dispatch delivered and acknowledged")
	return nil
}

// sendWithRetry retries a transient dispatch op a bounded number of times before
// surfacing the failure to the caller.
func (c Client) sendWithRetry(ctx context.Context, dispatchID string, op sendOp) (string, string, error) {
	var lastErr error
	for attempt := 0; attempt < maxSendAttempts; attempt++ {
		if attempt > 0 {
			select {
			case <-ctx.Done():
				return "", "", ctx.Err()
			case <-time.After(sendRetryDelay):
			}
		}
		messageID, threadID, err := op(ctx)
		if err == nil {
			return messageID, threadID, nil
		}
		lastErr = err
		c.logger.Warn().Err(err).Str("dispatch_id", dispatchID).Int("attempt", attempt+1).Msg("dispatch send attempt failed")
		if isPermanent(err) {
			return "", "", err
		}
	}
	return "", "", lastErr
}
