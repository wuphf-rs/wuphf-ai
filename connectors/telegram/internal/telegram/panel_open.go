package telegramconnector

import (
	"context"
	"errors"
	"fmt"
	"strconv"

	"github.com/gotd/td/tg"
	botconf "github.com/propheci/botconf/go/settings"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// panelContext names where a panel command was typed, in the core's terms:
// the chat as this connector reports chats (telegram:channel:N) and the forum
// topic. The core opens the panel on a page about it (plan 084: a pending
// post's preview, a Status topic's "Post a status").
func panelContext(peer tg.InputPeerClass, topic int) *botconf.Context {
	var chat string
	switch value := peer.(type) {
	case *tg.InputPeerUser:
		chat = formatPeer(&tg.PeerUser{UserID: value.UserID})
	case *tg.InputPeerChat:
		chat = formatPeer(&tg.PeerChat{ChatID: value.ChatID})
	case *tg.InputPeerChannel:
		chat = formatPeer(&tg.PeerChannel{ChannelID: value.ChannelID})
	default:
		return nil
	}
	context := &botconf.Context{Chat: chat}
	if topic > 0 {
		context.Thread = strconv.Itoa(topic)
	}
	return context
}

// OpenFor opens the panel in the chat (and topic) the core names, for actor
// (an owner id), as if they had typed the panel command there.
func (p *Panel) OpenFor(ctx context.Context, chatID, thread, actor string) error {
	front, _, _ := p.state()
	if front == nil {
		return errors.New("the settings panel has not started")
	}
	user, err := strconv.ParseInt(actor, 10, 64)
	if err != nil {
		return coreclient.Permanent(fmt.Errorf("the panel's actor %q is not a Telegram user id: %w", actor, err))
	}
	topic := 0
	if thread != "" {
		if topic, err = strconv.Atoi(thread); err != nil {
			return coreclient.Permanent(fmt.Errorf("invalid Telegram topic %q: %w", thread, err))
		}
	}
	peer, err := p.peers.InputPeer(ctx, chatID)
	if err != nil {
		return err
	}
	return front.open(ctx, peer, user, topic, "")
}

// panelOpener is what the sender needs to open the panel.
type panelOpener interface {
	OpenFor(ctx context.Context, chatID, thread, actor string) error
}

// WithPanel lets the sender open the settings panel when the core asks
// (OpenPanelDispatch).
func (s Sender) WithPanel(panel panelOpener) Sender {
	s.panel = panel
	return s
}

// OpenPanel opens the settings panel where the core asks (plan 084: the
// preview of the owner's pending post). Without a panel it only acknowledges.
// A failure to draw it is logged, not retried: the owner can still type the
// panel command, and a redelivered dispatch would post a second panel.
func (s Sender) OpenPanel(ctx context.Context, dispatch *wuphfpb.OpenPanelDispatch) (string, error) {
	if s.panel != nil {
		if err := s.panel.OpenFor(ctx, dispatch.GetTargetChatId(), dispatch.GetTargetThreadId(), dispatch.GetActor()); err != nil {
			s.peers.logger.Warn().Err(err).Str("chat", dispatch.GetTargetChatId()).Msg("could not open the settings panel for a post")
		}
	}
	return dispatch.GetTargetChatId(), nil
}
