package whatsappconnector

import (
	"context"
	"errors"
	"sync/atomic"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// errNotLinked fails a dispatch that arrives while no WhatsApp session is
// linked. It is transient, so the core delivers the dispatch once the new
// session is linked.
var errNotLinked = errors.New("WhatsApp is not linked: the connector is waiting to be linked again")

// linkedSender hands every dispatch to the Sender of the WhatsApp session
// that is running. The core session lives as long as the process; a logout
// replaces the WhatsApp session, and with it the Sender.
type linkedSender struct {
	current atomic.Pointer[Sender]
}

// set makes sender the one dispatches go to; nil while no session is linked.
func (l *linkedSender) set(sender *Sender) {
	l.current.Store(sender)
}

func (l *linkedSender) sender() (*Sender, error) {
	sender := l.current.Load()
	if sender == nil {
		return nil, errNotLinked
	}
	return sender, nil
}

func (l *linkedSender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, string, error) {
	sender, err := l.sender()
	if err != nil {
		return "", "", err
	}
	return sender.SendMessage(ctx, dispatch)
}

func (l *linkedSender) EditMessage(ctx context.Context, dispatch *wuphfpb.MessageEditDispatch) (string, error) {
	sender, err := l.sender()
	if err != nil {
		return "", err
	}
	return sender.EditMessage(ctx, dispatch)
}

func (l *linkedSender) DeleteMessage(ctx context.Context, dispatch *wuphfpb.MessageDeleteDispatch) (string, error) {
	sender, err := l.sender()
	if err != nil {
		return "", err
	}
	return sender.DeleteMessage(ctx, dispatch)
}

func (l *linkedSender) ApplyChatState(ctx context.Context, dispatch *wuphfpb.ChatStateDispatch) (string, error) {
	sender, err := l.sender()
	if err != nil {
		return "", err
	}
	return sender.ApplyChatState(ctx, dispatch)
}
