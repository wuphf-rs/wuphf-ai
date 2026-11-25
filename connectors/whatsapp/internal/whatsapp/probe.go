package whatsappconnector

import (
	"context"
	"errors"

	"github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// groupDirectory is what probing needs of the linked account: its own
// addresses and group metadata (a *whatsmeow.Client through whatsmeowProbe).
type groupDirectory interface {
	OwnPN() types.JID
	OwnLID() types.JID
	GetGroupInfo(ctx context.Context, jid types.JID) (*types.GroupInfo, error)
}

type whatsmeowProbe struct{ *whatsmeow.Client }

func (c whatsmeowProbe) OwnPN() types.JID  { return c.Store.GetJID() }
func (c whatsmeowProbe) OwnLID() types.JID { return c.Store.GetLID() }

// prober answers the core's health probes (plan 079): whether this account
// is in a group and may post there (an announce-only group takes an
// admin). WhatsApp has no topics. While no session is linked, every chat is
// unknown.
type prober struct {
	account func() groupDirectory
}

func newProber(client *linkedClient) prober {
	return prober{account: func() groupDirectory {
		if current := client.current.Load(); current != nil {
			return whatsmeowProbe{current}
		}
		return nil
	}}
}

func (p prober) Probe(ctx context.Context, chatIDs []string) []*wuphfpb.ChatFacts {
	account := p.account()
	if account == nil {
		return nil
	}
	facts := make([]*wuphfpb.ChatFacts, 0, len(chatIDs))
	for _, chatID := range chatIDs {
		facts = append(facts, probeChat(ctx, account, chatID))
	}
	return facts
}

func probeChat(ctx context.Context, account groupDirectory, chatID string) *wuphfpb.ChatFacts {
	facts := &wuphfpb.ChatFacts{
		ChatId:          chatID,
		Forum:           coreclient.Fact(false),
		CanCreateTopics: coreclient.Fact(false),
	}
	jid, err := types.ParseJID(chatID)
	if err != nil {
		facts.Detail = err.Error()
		return facts
	}
	switch {
	case isStatusBroadcast(jid):
		// The story feed takes no messages from the bridge (plan 083).
		facts.Exists, facts.Member, facts.CanSend = coreclient.Fact(true), coreclient.Fact(true), coreclient.Fact(false)
		facts.Detail = "WhatsApp Status takes no messages"
		return facts
	case jid.Server == types.NewsletterServer:
		// Whether a channel takes posts depends on this account's role,
		// which the posting facts report; the probe does not know it.
		return facts
	}
	if jid.Server != types.GroupServer {
		// A direct chat is always there to write to.
		facts.Exists, facts.Member, facts.CanSend = coreclient.Fact(true), coreclient.Fact(true), coreclient.Fact(true)
		return facts
	}
	info, err := account.GetGroupInfo(ctx, jid)
	switch {
	case errors.Is(err, whatsmeow.ErrGroupNotFound):
		facts.Exists, facts.Detail = coreclient.Fact(false), err.Error()
		return facts
	case errors.Is(err, whatsmeow.ErrNotInGroup):
		facts.Exists, facts.Member, facts.CanSend = coreclient.Fact(true), coreclient.Fact(false), coreclient.Fact(false)
		facts.Detail = err.Error()
		return facts
	case err != nil || info == nil:
		if err != nil {
			facts.Detail = err.Error()
		}
		return facts
	}
	own, member := ownParticipant(info, account.OwnPN(), account.OwnLID())
	admin := member && (own.IsAdmin || own.IsSuperAdmin)
	facts.Exists = coreclient.Fact(true)
	facts.Member = coreclient.Fact(member)
	facts.CanSend = coreclient.Fact(member && (!info.IsAnnounce || admin))
	if member && info.IsAnnounce && !admin {
		facts.Detail = "only admins can send messages"
	}
	facts.CanDeleteOthers = coreclient.Fact(admin)
	return facts
}

// ownParticipant finds this account among the group's participants, by
// phone number or by LID.
func ownParticipant(info *types.GroupInfo, pn, lid types.JID) (types.GroupParticipant, bool) {
	is := func(candidate, own types.JID) bool {
		return !own.IsEmpty() && !candidate.IsEmpty() && candidate.User == own.User
	}
	for _, participant := range info.Participants {
		for _, address := range []types.JID{participant.JID, participant.PhoneNumber, participant.LID} {
			if is(address, pn) || is(address, lid) {
				return participant, true
			}
		}
	}
	return types.GroupParticipant{}, false
}
