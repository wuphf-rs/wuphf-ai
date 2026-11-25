package whatsappconnector

import (
	"context"
	"slices"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// postingDirectory is what reporting posting facts needs of the linked
// account: its addresses, its groups and its channels (a *whatsmeow.Client
// through whatsmeowProbe).
type postingDirectory interface {
	groupDirectory
	GetJoinedGroups(ctx context.Context) ([]*types.GroupInfo, error)
	GetSubscribedNewsletters(ctx context.Context) ([]*types.NewsletterMetadata, error)
}

// postingReporter tells the core what this account may do in its chats
// (plan 083), so the core refuses what WhatsApp would not take, or would
// take wrongly: a message to status@broadcast is a story to every contact.
// It reports a snapshot each time the session connects, and a group again
// when its admin-only setting or this account's admin role changes.
type postingReporter struct {
	normalizer Normalizer
	directory  postingDirectory
	logger     zerolog.Logger
}

// statusPosting is the story feed's: no messages (a story is posted only on
// purpose), reactions (sent to the poster's DM), and a story is quoted only
// in its poster's chat.
func statusPosting() *wuphfpb.ChatPosting {
	return &wuphfpb.ChatPosting{Messages: false, Reactions: true, PrivateReplies: true}
}

// groupPosting is what this account may do in a group: anything, unless the
// group is admin-only (a community's announcement group is) and the account
// is not an admin there.
func groupPosting(info *types.GroupInfo, pn, lid types.JID) *wuphfpb.ChatPosting {
	own, member := ownParticipant(info, pn, lid)
	admin := member && (own.IsAdmin || own.IsSuperAdmin)
	return &wuphfpb.ChatPosting{Messages: !info.IsAnnounce || admin, Reactions: true}
}

// newsletterPosting is what this account may do in a channel: post only as
// its owner or an admin; react as anyone.
func newsletterPosting(channel *types.NewsletterMetadata) *wuphfpb.ChatPosting {
	role := types.NewsletterRole("")
	if channel.ViewerMeta != nil {
		role = channel.ViewerMeta.Role
	}
	owns := role == types.NewsletterRoleOwner || role == types.NewsletterRoleAdmin
	return &wuphfpb.ChatPosting{Messages: owns, Reactions: true}
}

// Snapshot reports the story feed, every joined group and every followed
// channel. A list that cannot be read is skipped (logged): the core keeps
// what it knew.
func (r postingReporter) Snapshot(ctx context.Context) error {
	updates := []*wuphfpb.ChatUpdate{{ChatId: types.StatusBroadcastJID.String(), Posting: statusPosting()}}
	if groups, err := r.directory.GetJoinedGroups(ctx); err != nil {
		r.logger.Warn().Err(err).Msg("could not list WhatsApp groups for their posting rights")
	} else {
		for _, group := range groups {
			updates = append(updates, &wuphfpb.ChatUpdate{
				ChatId:  r.normalizer.canonical(ctx, group.JID).String(),
				Posting: groupPosting(group, r.directory.OwnPN(), r.directory.OwnLID()),
			})
		}
	}
	if channels, err := r.directory.GetSubscribedNewsletters(ctx); err != nil {
		r.logger.Warn().Err(err).Msg("could not list WhatsApp channels for their posting rights")
	} else {
		for _, channel := range channels {
			updates = append(updates, &wuphfpb.ChatUpdate{ChatId: channel.ID.String(), Posting: newsletterPosting(channel)})
		}
	}
	return r.normalizer.sendChatUpdates(ctx, updates, true)
}

// GroupChanged reports a group again when a notification may have changed
// what this account may do there: the admin-only setting, or this account
// promoted or demoted.
func (r postingReporter) GroupChanged(ctx context.Context, evt *events.GroupInfo) error {
	if !r.affectsPosting(evt) {
		return nil
	}
	info, err := r.directory.GetGroupInfo(ctx, evt.JID)
	if err != nil || info == nil {
		r.logger.Warn().Err(err).Str("group", evt.JID.String()).Msg("could not read a WhatsApp group's posting rights")
		return nil
	}
	return r.normalizer.sendChatUpdates(ctx, []*wuphfpb.ChatUpdate{{
		ChatId:  r.normalizer.canonical(ctx, evt.JID).String(),
		Posting: groupPosting(info, r.directory.OwnPN(), r.directory.OwnLID()),
	}}, true)
}

func (r postingReporter) affectsPosting(evt *events.GroupInfo) bool {
	if evt.Announce != nil {
		return true
	}
	own := func(jid types.JID) bool {
		for _, address := range []types.JID{r.directory.OwnPN(), r.directory.OwnLID()} {
			if !address.IsEmpty() && jid.User == address.User {
				return true
			}
		}
		return false
	}
	return slices.ContainsFunc(evt.Promote, own) || slices.ContainsFunc(evt.Demote, own)
}
