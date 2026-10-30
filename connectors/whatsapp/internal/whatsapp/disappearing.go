package whatsappconnector

import (
	"context"
	"sync"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	waE2E "go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"
)

// GroupInfoClient looks a group up on WhatsApp; whatsmeow's client implements
// it. A seam so the timer source is testable without a session.
type GroupInfoClient interface {
	GetGroupInfo(ctx context.Context, jid types.JID) (*types.GroupInfo, error)
}

// timerSource decides the disappearing timer a send carries when the core's
// dispatch does not say: a group's timer is looked up (and reported to the
// core so it is known next time), and a DM's unknown timer is raised with the
// owner once per chat until a timer for it becomes known.
type timerSource struct {
	groups GroupInfoClient
	// inputs is the connector's frame channel to the core. The coreclient
	// session loop that calls SendMessage is the one draining it, so frames are
	// never sent blocking (see emit).
	inputs chan<- *wuphfpb.ClientFrame
	// life bounds the goroutines emit starts for a full channel.
	life   context.Context
	mu     sync.Mutex
	warned map[string]struct{}
}

func newTimerSource(groups GroupInfoClient, inputs chan<- *wuphfpb.ClientFrame, life context.Context) *timerSource {
	return &timerSource{groups: groups, inputs: inputs, life: life, warned: make(map[string]struct{})}
}

// For returns the timer to stamp on a send to chat (chatID is its canonical
// id, as the core knows it). A timer the core dispatched always wins.
func (t *timerSource) For(ctx context.Context, chatID string, chat types.JID, dispatched *wuphfpb.DisappearingTimer) *wuphfpb.DisappearingTimer {
	if dispatched != nil {
		t.known(chatID)
		return dispatched
	}
	if chat.Server == types.GroupServer {
		if info, err := t.groups.GetGroupInfo(ctx, chat); err == nil && info != nil {
			timer := &wuphfpb.DisappearingTimer{}
			if info.IsEphemeral {
				timer.Seconds = info.DisappearingTimer
			}
			t.known(chatID)
			t.emit(&wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_ChatUpdate{ChatUpdate: &wuphfpb.ChatUpdateEvent{
				Updates: []*wuphfpb.ChatUpdate{{ChatId: chatID, Disappearing: timer}},
			}}})
			return timer
		}
	}
	if t.firstWarning(chatID) {
		t.emit(&wuphfpb.ClientFrame{Body: &wuphfpb.ClientFrame_OwnerNotice{OwnerNotice: &wuphfpb.OwnerNotice{
			Kind:   wuphfpb.NoticeKind_NOTICE_KIND_DISAPPEARING_TIMER_UNKNOWN,
			Detail: "chat " + chatID,
		}}})
	}
	return nil
}

// firstWarning reports whether chatID has not been warned about since its
// timer was last known, recording the warning.
func (t *timerSource) firstWarning(chatID string) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if _, warned := t.warned[chatID]; warned {
		return false
	}
	t.warned[chatID] = struct{}{}
	return true
}

// known clears chatID's warning: a later unknown timer warns again.
func (t *timerSource) known(chatID string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	delete(t.warned, chatID)
}

// emit queues a frame for the core without blocking the caller: when the
// channel is full the frame waits on a goroutine of its own, until the
// connector stops.
func (t *timerSource) emit(frame *wuphfpb.ClientFrame) {
	select {
	case t.inputs <- frame:
	default:
		go func() {
			select {
			case t.inputs <- frame:
			case <-t.life.Done():
			}
		}()
	}
}

// withDisappearing puts the chat's timer on an outgoing content message. A nil
// or off timer leaves the message unchanged. The timer is merged into the
// content's existing ContextInfo (a reply's quote is kept), and a bare
// Conversation is promoted to an ExtendedTextMessage so it can carry one.
func withDisappearing(message *waE2E.Message, timer *wuphfpb.DisappearingTimer) *waE2E.Message {
	if timer.GetSeconds() == 0 {
		return message
	}
	if message.Conversation != nil {
		message = &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{Text: message.Conversation}}
	}
	var slot **waE2E.ContextInfo
	switch {
	case message.ExtendedTextMessage != nil:
		slot = &message.ExtendedTextMessage.ContextInfo
	case message.ImageMessage != nil:
		slot = &message.ImageMessage.ContextInfo
	case message.DocumentMessage != nil:
		slot = &message.DocumentMessage.ContextInfo
	case message.VideoMessage != nil:
		slot = &message.VideoMessage.ContextInfo
	case message.AudioMessage != nil:
		slot = &message.AudioMessage.ContextInfo
	case message.StickerMessage != nil:
		slot = &message.StickerMessage.ContextInfo
	default:
		return message
	}
	if *slot == nil {
		*slot = &waE2E.ContextInfo{}
	}
	(*slot).Expiration = proto.Uint32(timer.GetSeconds())
	if timer.SettingTimestamp != nil {
		(*slot).EphemeralSettingTimestamp = proto.Int64(timer.GetSettingTimestamp())
	}
	return message
}
