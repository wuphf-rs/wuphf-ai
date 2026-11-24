package telegramconnector

import (
	"context"
	"fmt"

	"github.com/gotd/td/tg"
	"github.com/gotd/td/tgerr"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// Prober answers the core's health probes (plan 079): what this account can
// do in a chat, from the chat as this session sees it. A lookup that fails
// for a reason other than the chat being gone leaves the facts unknown.
type Prober struct {
	api   *tg.Client
	peers *PeerCache
}

func NewProber(api *tg.Client, peers *PeerCache) Prober {
	return Prober{api: api, peers: peers}
}

func (p Prober) Probe(ctx context.Context, chatIDs []string) []*wuphfpb.ChatFacts {
	facts := make([]*wuphfpb.ChatFacts, 0, len(chatIDs))
	for _, chatID := range chatIDs {
		facts = append(facts, p.probe(ctx, chatID))
	}
	return facts
}

func (p Prober) probe(ctx context.Context, chatID string) *wuphfpb.ChatFacts {
	facts := &wuphfpb.ChatFacts{ChatId: chatID}
	kind, id, err := parseChatID(chatID)
	if err != nil {
		facts.Detail = err.Error()
		return facts
	}
	switch kind {
	case "user":
		// A private chat: this account is in it and may write (a block is
		// not visible from here).
		facts.Exists, facts.Member, facts.CanSend = coreclient.Fact(true), coreclient.Fact(true), coreclient.Fact(true)
		facts.Forum, facts.CanCreateTopics = coreclient.Fact(false), coreclient.Fact(false)
		return facts
	case "chat":
		chats, err := p.api.MessagesGetChats(ctx, []int64{id})
		if err != nil {
			return gone(facts, err)
		}
		for _, chat := range chats.GetChats() {
			if chat, ok := chat.(*tg.Chat); ok && chat.ID == id {
				basicGroupFacts(facts, chat)
				return facts
			}
		}
		facts.Exists = coreclient.Fact(false)
		return facts
	case "channel":
		peer, err := p.peers.InputPeer(ctx, chatID)
		if err != nil {
			// Not seen by this session yet: nothing to say.
			facts.Detail = err.Error()
			return facts
		}
		channelPeer, ok := peer.(*tg.InputPeerChannel)
		if !ok {
			facts.Detail = fmt.Sprintf("%s is not a channel", chatID)
			return facts
		}
		chats, err := p.api.ChannelsGetChannels(ctx, []tg.InputChannelClass{&tg.InputChannel{
			ChannelID:  channelPeer.ChannelID,
			AccessHash: channelPeer.AccessHash,
		}})
		if err != nil {
			return gone(facts, err)
		}
		for _, chat := range chats.GetChats() {
			switch chat := chat.(type) {
			case *tg.Channel:
				if chat.ID == id {
					channelFacts(facts, chat)
					return facts
				}
			case *tg.ChannelForbidden:
				if chat.ID == id {
					facts.Exists, facts.Member = coreclient.Fact(true), coreclient.Fact(false)
					facts.CanSend = coreclient.Fact(false)
					facts.Detail = "this account was removed from it"
					return facts
				}
			}
		}
		facts.Exists = coreclient.Fact(false)
		return facts
	default:
		facts.Detail = fmt.Sprintf("unknown Telegram chat kind %q", kind)
		return facts
	}
}

// gone reads a failed lookup: a chat that is invalid or private is gone (or
// out of reach); anything else is unknown.
func gone(facts *wuphfpb.ChatFacts, err error) *wuphfpb.ChatFacts {
	facts.Detail = err.Error()
	switch {
	case tgerr.Is(err, "CHANNEL_INVALID", "CHAT_ID_INVALID", "PEER_ID_INVALID"):
		facts.Exists = coreclient.Fact(false)
	case tgerr.Is(err, "CHANNEL_PRIVATE", "CHAT_FORBIDDEN", "USER_BANNED_IN_CHANNEL"):
		facts.Exists, facts.Member, facts.CanSend = coreclient.Fact(true), coreclient.Fact(false), coreclient.Fact(false)
	}
	return facts
}

// basicGroupFacts: a basic group has no topics.
func basicGroupFacts(facts *wuphfpb.ChatFacts, chat *tg.Chat) {
	admin, isAdmin := chat.GetAdminRights()
	defaults, _ := chat.GetDefaultBannedRights()
	privileged := chat.Creator || isAdmin
	facts.Exists = coreclient.Fact(!chat.Deactivated)
	facts.Member = coreclient.Fact(!chat.Left)
	facts.CanSend = coreclient.Fact(!chat.Left && (privileged || !defaults.SendMessages))
	facts.Forum = coreclient.Fact(false)
	facts.CanCreateTopics = coreclient.Fact(false)
	facts.CanPin = coreclient.Fact(chat.Creator || admin.PinMessages || !defaults.PinMessages)
	facts.CanDeleteOthers = coreclient.Fact(chat.Creator || admin.DeleteMessages)
}

// channelFacts covers supergroups (forums among them) and broadcast
// channels. A right is held by the creator, an admin granted it, or (in a
// supergroup) everyone when neither the group's defaults nor this
// account's own restrictions take it away.
func channelFacts(facts *wuphfpb.ChatFacts, chat *tg.Channel) {
	admin, isAdmin := chat.GetAdminRights()
	defaults, _ := chat.GetDefaultBannedRights()
	own, _ := chat.GetBannedRights()
	everyone := func(banned func(tg.ChatBannedRights) bool) bool {
		return chat.Megagroup && !banned(defaults) && !banned(own)
	}
	facts.Exists = coreclient.Fact(true)
	facts.Member = coreclient.Fact(!chat.Left)
	canSend := chat.Creator || (isAdmin && (chat.Megagroup || admin.PostMessages)) ||
		everyone(func(rights tg.ChatBannedRights) bool { return rights.SendMessages })
	facts.CanSend = coreclient.Fact(!chat.Left && canSend)
	facts.Forum = coreclient.Fact(chat.Forum)
	facts.CanCreateTopics = coreclient.Fact(chat.Forum && (chat.Creator || admin.ManageTopics ||
		everyone(func(rights tg.ChatBannedRights) bool { return rights.ManageTopics })))
	facts.CanPin = coreclient.Fact(chat.Creator || admin.PinMessages ||
		everyone(func(rights tg.ChatBannedRights) bool { return rights.PinMessages }))
	facts.CanDeleteOthers = coreclient.Fact(chat.Creator || admin.DeleteMessages)
}
