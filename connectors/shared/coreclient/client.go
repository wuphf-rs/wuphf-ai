// Package coreclient is the shared connector session client. Every Go connector
// maintains its session against the core through this package: the
// reconnect-with-backoff loop, bounded send retry, permanent-error
// dead-lettering, and frame dispatch are identical across platforms and live
// here once. Per-connector facts (platform, capabilities, the optional
// platform-specific permanent-error classifier) are supplied through Options,
// and platform-specific dispatch handling (retitle, reaction, delivery-ack,
// seen-receipt) is opted into by implementing the corresponding capability
// interface on the Sender.
package coreclient

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

const (
	initialBackoff  = time.Second
	maxBackoff      = 30 * time.Second
	maxSendAttempts = 3
	sendRetryDelay  = 500 * time.Millisecond
	// maxRetryAfter caps a platform's asked-for wait (RetryAfter), so a very
	// long one still retries, at that pace, rather than holding the session.
	maxRetryAfter = 5 * time.Minute
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

// isPermanent reports whether err was explicitly marked Permanent. Callers that
// also honor a connector's platform-specific classifier use Client.isPermanent.
func isPermanent(err error) bool {
	var marked PermanentError
	return errors.As(err, &marked)
}

// Options carries the per-connector facts the shared session client needs. Each
// connector maps its own config into this struct; the shared client imports no
// connector's config package.
type Options struct {
	CoreAddress         string
	ConnectorID         string
	Platform            wuphfpb.Platform
	SupportsThreads     bool
	SupportsTopicCreate bool
	// SupportsDisappearingTimers declares that the connector stamps chats'
	// disappearing-messages timers on what it sends and reports the timers it
	// observes. Part of the registration, so it must match the connector's
	// entry in the core config.
	SupportsDisappearingTimers bool
	// Token is the connector's core token, presented on the session as
	// `authorization: Bearer <token>`.
	Token string
	// IsPermanent, when set, classifies platform-specific errors as permanent in
	// addition to the coreclient.Permanent wrapping (e.g. a Telegram RPC 400).
	IsPermanent func(error) bool
	// RetryAfter, when set, reads how long the platform asked to wait before
	// trying again (e.g. a Telegram FLOOD_WAIT). The retry waits that long
	// (up to five minutes) instead of the short fixed delay, so a rate limit
	// is waited out instead of ending the session and being redelivered into
	// the same limit.
	RetryAfter func(error) (time.Duration, bool)
	// Settings, when set, declares the connector's runtime settings in the
	// registration (see the settings package), so the core keeps and pushes
	// them. Nil declares none, and the core keeps any section declared before.
	Settings *wuphfpb.ConnectorSettingsDeclaration
	// Commands, when set, are what the owner can run on the connector from the
	// settings panel (see Commands): declared at registration, run on the
	// core's RunCommand. Nil declares none.
	Commands Commands
	// Prober, when set, answers the core's health probes (see Prober). Nil
	// answers every chat as unknown.
	Prober Prober
	// Tools, when set, reports the external programs the connector runs, at
	// registration and in every probe's answer.
	Tools Tools
}

// Sender is the minimum surface every connector implements: the three message
// operations the core dispatches to every platform. Connectors that also mirror
// retitles, reactions, delivery acks, or seen receipts implement the matching
// capability interface below; the session loop routes those frames to the
// implemented method and otherwise settles them (see runSession).
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

// ThreadRetitler is implemented by senders on platforms with renameable mirror
// threads. A MessageRetitle dispatch reaching a sender without it is
// dead-lettered as permanently unsupported (a safety net — the core only routes
// retitles to mirror-capable connectors).
type ThreadRetitler interface {
	// RetitleThread renames an already-provisioned mirror thread and returns the
	// thread id, which the retitle leaves unchanged.
	RetitleThread(context.Context, *wuphfpb.MessageRetitleDispatch) (threadID string, err error)
}

// Reactor is implemented by senders that mirror reactions onto delivered copies.
// A Reaction dispatch reaching a sender without it is acknowledged unchanged so
// the core stops redelivering it.
type Reactor interface {
	// ApplyReaction reflects a reaction change on an already-delivered copy and
	// returns the reacted copy's platform message id, which is left unchanged so
	// the reaction does not disturb the copy mapping.
	ApplyReaction(context.Context, *wuphfpb.ReactionDispatch) (messageID string, err error)
}

// DeliveryAcker is implemented by senders that surface delivery acks (a sent
// message reached the source). Without it, a DeliveryAck dispatch is acknowledged
// unchanged.
type DeliveryAcker interface {
	// AckDelivery surfaces that a sent message reached the source and returns the
	// target copy's platform message id unchanged.
	AckDelivery(context.Context, *wuphfpb.DeliveryAckDispatch) (messageID string, err error)
}

// SeenReceiptMarker is implemented by senders that surface read receipts.
// Without it, a SeenReceipt dispatch is acknowledged unchanged.
type SeenReceiptMarker interface {
	// MarkSeen surfaces that a sent message was read and returns the target copy's
	// platform message id unchanged.
	MarkSeen(context.Context, *wuphfpb.SeenReceiptDispatch) (messageID string, err error)
}

// ChatStateApplier is implemented by senders that apply pin, mute and archive
// to a chat or thread. Without it, a ChatState dispatch is acknowledged
// unchanged (with its target chat id). Applying a state emits nothing: the
// platform's own echo comes back through the event handlers.
type ChatStateApplier interface {
	// ApplyChatState brings the target chat (or thread) to the dispatch's
	// state, leaving absent fields alone, and returns the target chat id. A
	// field the platform cannot apply is acknowledged without acting.
	ApplyChatState(context.Context, *wuphfpb.ChatStateDispatch) (chatID string, err error)
}

// sendOp performs one outbound delivery attempt, returning the resulting
// platform message id and, when a thread was provisioned, its id.
type sendOp func(context.Context) (messageID string, threadID string, err error)

// ackOnly is a send op that performs no platform action and acknowledges the
// dispatch with the given target id. It settles signals the connector chooses
// not to render, so the core stops redelivering them.
func ackOnly(targetMessageID string) sendOp {
	return func(context.Context) (string, string, error) { return targetMessageID, "", nil }
}

// unsupportedOp is a send op that permanently fails, so a dispatch a connector
// cannot render is dead-lettered rather than retried or redelivered forever.
func unsupportedOp(reason string) sendOp {
	return func(context.Context) (string, string, error) { return "", "", Permanent(errors.New(reason)) }
}

type Client struct {
	opts   Options
	logger zerolog.Logger
}

func New(opts Options, logger zerolog.Logger) Client {
	return Client{opts: opts, logger: logger.With().Str("component", "core-client").Logger()}
}

// isPermanent reports whether a send failure should dead-letter the dispatch: it
// is either explicitly marked Permanent or classified permanent by the
// connector's optional platform-specific hook.
func (c Client) isPermanent(err error) bool {
	if isPermanent(err) {
		return true
	}
	return c.opts.IsPermanent != nil && c.opts.IsPermanent(err)
}

// retryDelay is how long to wait before retrying after err: what the platform
// asked for (Options.RetryAfter, capped at maxRetryAfter), else sendRetryDelay.
func (c Client) retryDelay(err error) time.Duration {
	if c.opts.RetryAfter == nil {
		return sendRetryDelay
	}
	wait, ok := c.opts.RetryAfter(err)
	if !ok {
		return sendRetryDelay
	}
	return min(max(wait, sendRetryDelay), maxRetryAfter)
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
	connection, err := grpc.NewClient(
		c.opts.CoreAddress,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		WithToken(c.opts.Token),
	)
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
			ConnectorId: c.opts.ConnectorID,
			Platform:    c.opts.Platform,
			Capabilities: &wuphfpb.Capabilities{
				SupportsThreads:            c.opts.SupportsThreads,
				SupportsTopicCreate:        c.opts.SupportsTopicCreate,
				SupportsDisappearingTimers: c.opts.SupportsDisappearingTimers,
			},
			Settings: c.opts.Settings,
			Commands: declarations(c.opts.Commands),
			Tools:    toolsNow(c.opts.Tools),
		}},
	}); err != nil {
		return fmt.Errorf("register connector: %w", err)
	}
	onConnected()
	c.logger.Info().Str("connector_id", c.opts.ConnectorID).Msg("connector session registered with core")

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

	// Commands run in their own goroutines, so a slow one never holds up
	// dispatches; their results are written here, as a stream takes one
	// writer.
	commandResults := make(chan *wuphfpb.ClientFrame)

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case result := <-commandResults:
			if err := stream.Send(result); err != nil {
				return fmt.Errorf("send a command result to core: %w", err)
			}
		case err := <-receiveErrors:
			return fmt.Errorf("receive core dispatch: %w", err)
		case input := <-inputs:
			if err := stream.Send(input); err != nil {
				return fmt.Errorf("send connector input to core: %w", err)
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
			case *wuphfpb.ServerFrame_MessageRetitle:
				dispatch := body.MessageRetitle
				op := unsupportedOp("connector does not support thread retitling")
				if retitler, ok := sender.(ThreadRetitler); ok {
					op = func(ctx context.Context) (string, string, error) {
						threadID, err := retitler.RetitleThread(ctx, dispatch)
						return threadID, "", err
					}
				}
				if err := c.deliver(ctx, stream, dispatch.DispatchId, op); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_Reaction:
				dispatch := body.Reaction
				op := ackOnly(dispatch.TargetPlatformMessageId)
				if reactor, ok := sender.(Reactor); ok {
					op = func(ctx context.Context) (string, string, error) {
						messageID, err := reactor.ApplyReaction(ctx, dispatch)
						return messageID, "", err
					}
				}
				if err := c.deliver(ctx, stream, dispatch.DispatchId, op); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_DeliveryAck:
				dispatch := body.DeliveryAck
				op := ackOnly(dispatch.TargetPlatformMessageId)
				if acker, ok := sender.(DeliveryAcker); ok {
					op = func(ctx context.Context) (string, string, error) {
						messageID, err := acker.AckDelivery(ctx, dispatch)
						return messageID, "", err
					}
				}
				if err := c.deliver(ctx, stream, dispatch.DispatchId, op); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_SeenReceipt:
				dispatch := body.SeenReceipt
				op := ackOnly(dispatch.TargetPlatformMessageId)
				if marker, ok := sender.(SeenReceiptMarker); ok {
					op = func(ctx context.Context) (string, string, error) {
						messageID, err := marker.MarkSeen(ctx, dispatch)
						return messageID, "", err
					}
				}
				if err := c.deliver(ctx, stream, dispatch.DispatchId, op); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_ChatState:
				dispatch := body.ChatState
				op := ackOnly(dispatch.TargetChatId)
				if applier, ok := sender.(ChatStateApplier); ok {
					op = func(ctx context.Context) (string, string, error) {
						chatID, err := applier.ApplyChatState(ctx, dispatch)
						return chatID, "", err
					}
				}
				if err := c.deliver(ctx, stream, dispatch.DispatchId, op); err != nil {
					return err
				}
			case *wuphfpb.ServerFrame_RunCommand:
				command := body.RunCommand
				go func() {
					result := answerCommand(ctx, c.opts.Commands, command)
					select {
					case commandResults <- result:
					case <-ctx.Done():
					}
				}()
			// Probes look chats up on the platform: in their own goroutines,
			// answered through the same writer as commands.
			case *wuphfpb.ServerFrame_Probe:
				probe := body.Probe
				go func() {
					result := answerProbe(ctx, c.opts.Prober, c.opts.Tools, probe)
					select {
					case commandResults <- result:
					case <-ctx.Done():
					}
				}()
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
		if !c.isPermanent(err) {
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
// surfacing the failure to the caller. Between attempts it waits as long as the
// platform asked (retryDelay).
func (c Client) sendWithRetry(ctx context.Context, dispatchID string, op sendOp) (string, string, error) {
	var lastErr error
	for attempt := 0; attempt < maxSendAttempts; attempt++ {
		if attempt > 0 {
			select {
			case <-ctx.Done():
				return "", "", ctx.Err()
			case <-time.After(c.retryDelay(lastErr)):
			}
		}
		messageID, threadID, err := op(ctx)
		if err == nil {
			return messageID, threadID, nil
		}
		lastErr = err
		c.logger.Warn().Err(err).Str("dispatch_id", dispatchID).Int("attempt", attempt+1).Msg("dispatch send attempt failed")
		if c.isPermanent(err) {
			return "", "", err
		}
	}
	return "", "", lastErr
}
