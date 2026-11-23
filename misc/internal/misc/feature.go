// Package misc hosts features that are not bridging: each watches the core's
// live message stream and may reply into the chat a message came from. The
// core stays a broker; a feature is a client of its observer and function
// services, like the TUI.
package misc

import (
	"context"
	"strings"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// ClientName tags wuphf-misc's sends (SendMessageRequest.client). The core
// stores it as the outgoing row's sent_by, and the runner skips those rows, so
// a feature never reacts to its own replies.
const ClientName = "wuphf-misc"

// Direction says whether a message arrived from the platform or went out
// through the core (a TUI send, a feature's reply).
type Direction int

const (
	Incoming Direction = iota + 1
	Outgoing
)

// Message is one live message a feature may act on.
type Message struct {
	ConnectorID string
	ChatID      string
	// ThreadID is empty for a chat without threads.
	ThreadID   string
	NativeID   string
	Sender     string
	SenderName string
	Text       string
	Direction  Direction
	// SentBy names the client that sent an outgoing message ("" when none
	// did, or for incoming messages).
	SentBy      string
	TimestampMs int64
}

// Chat is the message's chat, and its thread when it has one.
func (m Message) Chat() ChatRef {
	return ChatRef{ConnectorID: m.ConnectorID, ChatID: m.ChatID, ThreadID: m.ThreadID}
}

func (m Message) key() string {
	return m.ConnectorID + "\x00" + m.ChatID + "\x00" + m.NativeID
}

func messageFromProto(observed *wuphfpb.ObservedMessage) Message {
	direction := Direction(0)
	switch observed.GetDirection() {
	case wuphfpb.ObservedDirection_OBSERVED_DIRECTION_INCOMING:
		direction = Incoming
	case wuphfpb.ObservedDirection_OBSERVED_DIRECTION_OUTGOING:
		direction = Outgoing
	}
	return Message{
		ConnectorID: observed.GetConnectorId(),
		ChatID:      observed.GetChatId(),
		ThreadID:    observed.GetThreadId(),
		NativeID:    observed.GetPlatformMessageId(),
		Sender:      observed.GetSender(),
		SenderName:  observed.GetSenderName(),
		Text:        observed.GetText(),
		Direction:   direction,
		SentBy:      observed.GetSentBy(),
		TimestampMs: observed.GetTimestampMs(),
	}
}

// Feature is one non-bridging feature. Handle is called once per live message
// that may trigger features (see Runner), each call on its own goroutine, so a
// slow feature never stalls the stream. Handle returns when its work is done;
// it must stop early when ctx ends.
type Feature interface {
	Name() string
	Handle(ctx context.Context, message Message, out Sender)
}

// Sender is the only way a feature posts.
type Sender interface {
	// Reply sends text and the stored media mediaIDs into to's chat (and
	// thread), as a reply to it. Text may be empty when mediaIDs is not.
	Reply(ctx context.Context, to Message, text string, mediaIDs []string) error
}

// ChatRef names a chat, and optionally one thread in it, in the core's
// `connector:chat#thread` form.
type ChatRef struct {
	ConnectorID string
	ChatID      string
	ThreadID    string
}

// ParseChatRef parses `connector:chat` or `connector:chat#thread`. The chat id
// may contain `:`; the last `#` starts the thread. `%23` and `%25` stand for
// `#` and `%`.
func ParseChatRef(text string) (ChatRef, bool) {
	connector, rest, ok := strings.Cut(text, ":")
	if !ok || connector == "" {
		return ChatRef{}, false
	}
	chat, thread, hasThread := rest, "", false
	if index := strings.LastIndex(rest, "#"); index >= 0 {
		chat, thread, hasThread = rest[:index], rest[index+1:], true
	}
	if chat == "" || (hasThread && thread == "") {
		return ChatRef{}, false
	}
	chat, okChat := unescape(chat)
	thread, okThread := unescape(thread)
	if !okChat || !okThread {
		return ChatRef{}, false
	}
	return ChatRef{ConnectorID: connector, ChatID: chat, ThreadID: thread}, true
}

func unescape(part string) (string, bool) {
	var decoded strings.Builder
	for {
		index := strings.IndexByte(part, '%')
		if index < 0 {
			decoded.WriteString(part)
			return decoded.String(), true
		}
		decoded.WriteString(part[:index])
		if len(part) < index+3 {
			return "", false
		}
		switch part[index : index+3] {
		case "%23":
			decoded.WriteByte('#')
		case "%25":
			decoded.WriteByte('%')
		default:
			return "", false
		}
		part = part[index+3:]
	}
}

// Covers reports whether a message in chat falls under ref: the same chat, and
// the same thread when ref names one (a ref without a thread covers every
// thread of its chat).
func (ref ChatRef) Covers(chat ChatRef) bool {
	return ref.ConnectorID == chat.ConnectorID && ref.ChatID == chat.ChatID &&
		(ref.ThreadID == "" || ref.ThreadID == chat.ThreadID)
}
