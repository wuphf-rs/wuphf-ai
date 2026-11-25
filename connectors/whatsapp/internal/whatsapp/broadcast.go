package whatsappconnector

import (
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// broadcastTag follows the sender's name on a message that came through a
// broadcast list (plan 085).
const broadcastTag = "[broadcast]"

// broadcastCopies is where a broadcast-list message belongs (plan 085).
// WhatsApp shows a broadcast in the direct chat with each person, never as a
// chat of its own, so the bridge does too:
//
//   - one sent to the owner goes to the sender's DM;
//   - one the owner sent from the phone goes to the DM of each person it was
//     sent to (none when WhatsApp names none);
//   - anything else (not a broadcast list; status@broadcast is not one) is
//     itself.
//
// Each copy is the message with its chat moved, so edits, deletes and
// reactions of a broadcast land in the same DMs. broadcast says the copies
// came out of a broadcast list.
func broadcastCopies(message *events.Message) (copies []*events.Message, broadcast bool) {
	if !message.Info.Chat.IsBroadcastList() {
		return []*events.Message{message}, false
	}
	if !message.Info.IsFromMe {
		return []*events.Message{inChat(message, message.Info.Sender.ToNonAD())}, true
	}
	copies = make([]*events.Message, 0, len(message.Info.BroadcastRecipients))
	for _, recipient := range message.Info.BroadcastRecipients {
		chat := recipient.LID
		if chat.IsEmpty() {
			chat = recipient.PN
		}
		if !chat.IsEmpty() {
			copies = append(copies, inChat(message, chat.ToNonAD()))
		}
	}
	return copies, true
}

// inChat is message moved to chat.
func inChat(message *events.Message, chat types.JID) *events.Message {
	moved := *message
	moved.Info.Chat = chat
	moved.Info.IsGroup = false
	moved.Info.BroadcastListOwner = types.EmptyJID
	moved.Info.BroadcastRecipients = nil
	return &moved
}

// tagBroadcast puts the broadcast tag after the sender's name.
func tagBroadcast(event *wuphfpb.MessageCreateEvent) {
	name := broadcastTag
	if event.GetSenderName() != "" {
		name = event.GetSenderName() + " " + broadcastTag
	}
	event.SenderName = &name
}
