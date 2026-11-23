package coreclient

import (
	"context"
	"fmt"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// Commands are what the owner can run on a connector from the core's settings
// panel (plan 058), e.g. a resync: the connector declares them when it
// registers (Options.Commands), the core offers them as buttons on the
// connector's section while the session is live, and a press arrives as a
// RunCommand, answered with a CommandResult. The core decides nothing about
// them; the connector runs what it declared.
//
// Answer quickly: the core waits a few seconds for the result, so start slow
// work in the background and say so.
type Commands interface {
	// Declarations are the commands to offer, in the order the panel lists
	// them.
	Declarations() []*wuphfpb.ConnectorCommand
	// Run runs the declared command id and returns what to tell the owner, or
	// why it failed.
	Run(ctx context.Context, id string) (string, error)
}

// answerCommand runs command (unless it was not declared) and returns its
// CommandResult frame.
func answerCommand(ctx context.Context, commands Commands, command *wuphfpb.RunCommand) *wuphfpb.ClientFrame {
	message, err := runCommand(ctx, commands, command.GetCommandId())
	if err != nil {
		message = err.Error()
	}
	return &wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_CommandResult{CommandResult: &wuphfpb.CommandResult{
		RequestId: command.GetRequestId(),
		Ok:        err == nil,
		Message:   message,
	}}}
}

func runCommand(ctx context.Context, commands Commands, id string) (string, error) {
	if commands == nil {
		return "", fmt.Errorf("this connector runs no commands")
	}
	for _, declared := range commands.Declarations() {
		if declared.GetId() == id {
			return commands.Run(ctx, id)
		}
	}
	return "", fmt.Errorf("unknown command %q", id)
}

// declarations are the commands to declare at registration.
func declarations(commands Commands) []*wuphfpb.ConnectorCommand {
	if commands == nil {
		return nil
	}
	return commands.Declarations()
}
