// Serving remote functions. A provider dials the core, registers its functions
// with a ProviderHello and answers the calls the core brokers to it. Serve owns
// the whole session lifecycle: it runs each call's handler concurrently under
// the call's deadline, cancels handlers the core cancels, funnels results
// through a single sender, and reconnects after transient failures.

package functions

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"sync"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

const defaultReconnectDelay = 2 * time.Second

// Call is one invocation the core brokered to this provider.
type Call struct {
	ID       string
	Caller   string
	Args     json.RawMessage
	MediaIDs []string
}

// Result is a handler's successful outcome.
type Result struct {
	Body     any // marshalled to JSON
	MediaIDs []string
}

// Handler serves one function. ctx carries the call's deadline and is
// cancelled when the caller gives up or the session ends.
type Handler func(ctx context.Context, call Call) (Result, error)

// Error lets a handler choose the error code the caller sees. Any other error
// is reported as CALL_ERROR_CODE_FAILED.
type Error struct {
	Code    wuphfpb.CallErrorCode
	Message string
}

func (e *Error) Error() string { return e.Message }

// InvalidArgument reports that the call's args or media are unusable.
func InvalidArgument(format string, args ...any) error {
	return &Error{Code: wuphfpb.CallErrorCode_CALL_ERROR_CODE_INVALID_ARGUMENT, Message: fmt.Sprintf(format, args...)}
}

// Unsupported reports a well-formed request this provider cannot serve.
func Unsupported(format string, args ...any) error {
	return &Error{Code: wuphfpb.CallErrorCode_CALL_ERROR_CODE_UNSUPPORTED, Message: fmt.Sprintf(format, args...)}
}

// Function declares one function and its handler.
type Function struct {
	Name            string
	Major           uint32
	RequestSchema   string // JSON Schema document
	ResponseSchema  string
	Idempotent      bool
	MaxConcurrency  uint32
	DefaultDeadline time.Duration // 0 = let the core decide
	Handle          Handler
}

type ProviderOptions struct {
	Address        string
	ProviderID     string
	Token          string
	Functions      []Function
	ReconnectDelay time.Duration // 0 = 2s

	// dialOptions are appended to the connection's options (tests dial an
	// in-memory listener).
	dialOptions []grpc.DialOption
}

type functionKey struct {
	name  string
	major uint32
}

// Serve registers the functions and serves calls until ctx ends, reconnecting
// after transient failures. It returns at once on a rejection that retrying
// cannot fix: Unauthenticated, PermissionDenied or InvalidArgument.
func Serve(ctx context.Context, opts ProviderOptions, logger zerolog.Logger) error {
	if len(opts.Functions) == 0 {
		return errors.New("serve: no functions to register")
	}
	delay := opts.ReconnectDelay
	if delay == 0 {
		delay = defaultReconnectDelay
	}
	handlers := make(map[functionKey]Handler, len(opts.Functions))
	for _, function := range opts.Functions {
		handlers[functionKey{function.Name, function.Major}] = function.Handle
	}
	options := append([]grpc.DialOption{grpc.WithTransportCredentials(insecure.NewCredentials())}, opts.dialOptions...)
	connection, err := grpc.NewClient(opts.Address, options...)
	if err != nil {
		return fmt.Errorf("connect to core provider service: %w", err)
	}
	defer connection.Close()
	session := providerSession{
		client:   wuphfpb.NewProviderTransportClient(connection),
		opts:     opts,
		handlers: handlers,
		logger:   logger,
	}

	for {
		err := session.run(ctx)
		if ctx.Err() != nil {
			return ctx.Err()
		}
		switch status.Code(err) {
		case codes.Unauthenticated, codes.PermissionDenied, codes.InvalidArgument:
			return fmt.Errorf("core rejected provider %s: %w", opts.ProviderID, err)
		}
		logger.Warn().Err(err).Dur("retry_in", delay).Msg("provider session ended; reconnecting")
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(delay):
		}
	}
}

type providerSession struct {
	client   wuphfpb.ProviderTransportClient
	opts     ProviderOptions
	handlers map[functionKey]Handler
	logger   zerolog.Logger
}

// run serves one session until it ends and returns why it ended.
func (s providerSession) run(ctx context.Context) error {
	sessionCtx, endSession := context.WithCancel(ctx)
	defer endSession()
	stream, err := s.client.OpenProviderSession(
		metadata.AppendToOutgoingContext(sessionCtx, "authorization", "Bearer "+s.opts.Token),
	)
	if err != nil {
		return err
	}
	if err := stream.Send(&wuphfpb.ProviderClientFrame{
		Body: &wuphfpb.ProviderClientFrame_Hello{Hello: s.hello()},
	}); err != nil && !errors.Is(err, io.EOF) {
		// io.EOF means the core already ended the stream; Recv reports why.
		return err
	}

	// gRPC client streams are not safe for concurrent Send, so every result
	// goes through this one sender.
	results := make(chan *wuphfpb.CallResult)
	go func() {
		for {
			select {
			case <-sessionCtx.Done():
				return
			case result := <-results:
				if err := stream.Send(&wuphfpb.ProviderClientFrame{
					Body: &wuphfpb.ProviderClientFrame_Result{Result: result},
				}); err != nil {
					endSession()
					return
				}
			}
		}
	}()

	var mu sync.Mutex
	running := make(map[string]context.CancelFunc)
	for {
		frame, err := stream.Recv()
		if err != nil {
			if errors.Is(err, io.EOF) {
				return errors.New("core ended the provider session")
			}
			return err
		}
		switch body := frame.GetBody().(type) {
		case *wuphfpb.ProviderServerFrame_Call:
			call := body.Call
			var callCtx context.Context
			var cancel context.CancelFunc
			if call.GetDeadlineUnixMs() > 0 {
				callCtx, cancel = context.WithDeadline(sessionCtx, time.UnixMilli(call.GetDeadlineUnixMs()))
			} else {
				callCtx, cancel = context.WithCancel(sessionCtx)
			}
			mu.Lock()
			running[call.GetCallId()] = cancel
			mu.Unlock()
			go func() {
				defer func() {
					mu.Lock()
					delete(running, call.GetCallId())
					mu.Unlock()
					cancel()
				}()
				result := s.execute(callCtx, call)
				select {
				case results <- result:
				case <-sessionCtx.Done():
				}
			}()
		case *wuphfpb.ProviderServerFrame_Cancel:
			mu.Lock()
			cancel := running[body.Cancel.GetCallId()]
			mu.Unlock()
			if cancel != nil {
				cancel()
			}
		}
	}
}

func (s providerSession) hello() *wuphfpb.ProviderHello {
	declarations := make([]*wuphfpb.FunctionDeclaration, 0, len(s.opts.Functions))
	for _, function := range s.opts.Functions {
		declaration := &wuphfpb.FunctionDeclaration{
			Function:       &wuphfpb.FunctionRef{Name: function.Name, Major: function.Major},
			RequestSchema:  function.RequestSchema,
			ResponseSchema: function.ResponseSchema,
			Idempotent:     function.Idempotent,
			MaxConcurrency: function.MaxConcurrency,
		}
		if function.DefaultDeadline > 0 {
			milliseconds := uint32(function.DefaultDeadline.Milliseconds())
			declaration.DefaultDeadlineMs = &milliseconds
		}
		declarations = append(declarations, declaration)
	}
	return &wuphfpb.ProviderHello{ProviderId: s.opts.ProviderID, Functions: declarations}
}

// execute runs the call's handler and turns its outcome, error or panic into
// a CallResult.
func (s providerSession) execute(ctx context.Context, call *wuphfpb.ProviderCall) (result *wuphfpb.CallResult) {
	function := call.GetFunction()
	logger := s.logger.With().Str("call", call.GetCallId()).Str("function", fmt.Sprintf("%s@%d", function.GetName(), function.GetMajor())).Str("caller", call.GetCaller()).Logger()
	failed := func(code wuphfpb.CallErrorCode, message string) *wuphfpb.CallResult {
		logger.Warn().Str("code", code.String()).Str("error", message).Msg("function call failed")
		return &wuphfpb.CallResult{
			CallId:  call.GetCallId(),
			Outcome: &wuphfpb.CallResult_Error{Error: &wuphfpb.CallError{Code: code, Message: message}},
		}
	}
	handle := s.handlers[functionKey{function.GetName(), function.GetMajor()}]
	if handle == nil {
		return failed(wuphfpb.CallErrorCode_CALL_ERROR_CODE_UNSUPPORTED,
			fmt.Sprintf("provider %s does not serve %s@%d", s.opts.ProviderID, function.GetName(), function.GetMajor()))
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			result = failed(wuphfpb.CallErrorCode_CALL_ERROR_CODE_FAILED, fmt.Sprintf("handler panicked: %v", recovered))
		}
	}()

	outcome, err := handle(ctx, Call{
		ID:       call.GetCallId(),
		Caller:   call.GetCaller(),
		Args:     call.GetArgs(),
		MediaIDs: call.GetMediaIds(),
	})
	if err != nil {
		var coded *Error
		if errors.As(err, &coded) {
			return failed(coded.Code, coded.Message)
		}
		return failed(wuphfpb.CallErrorCode_CALL_ERROR_CODE_FAILED, err.Error())
	}
	body, err := json.Marshal(outcome.Body)
	if err != nil {
		return failed(wuphfpb.CallErrorCode_CALL_ERROR_CODE_FAILED, fmt.Sprintf("encode result: %v", err))
	}
	logger.Debug().Msg("function call completed")
	return &wuphfpb.CallResult{
		CallId:  call.GetCallId(),
		Outcome: &wuphfpb.CallResult_Success{Success: &wuphfpb.CallSuccess{Result: body, MediaIds: outcome.MediaIDs}},
	}
}
