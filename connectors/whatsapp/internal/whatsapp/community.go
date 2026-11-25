package whatsappconnector

import (
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// communitySeparator joins a community's name and its group's in a title.
const communitySeparator = " › "

// groupNamer looks up a group's name by its address ("" when unknown).
type groupNamer func(group types.JID) string

// communityName is a group's plain name inside its community (plan 085):
// "Community › Group". A community whose name is unknown leaves the group's.
func communityName(community, group string) string {
	if community == "" {
		return group
	}
	return community + communitySeparator + group
}

// groupTitle is a group's display title: "[G] Group", or "[G] Community ›
// Group" for a group in a community. A community itself is "[G] Community".
func groupTitle(info *types.GroupInfo, name groupNamer) string {
	if info == nil {
		return ""
	}
	community := ""
	if parent := parentOf(info); !parent.IsEmpty() {
		community = name(parent)
	}
	return tagged("G", communityName(community, info.Name))
}

// groupCommunity is the community a group belongs to, as the core is told
// it: the parent's id and name, or an empty one when it belongs to none.
func groupCommunity(info *types.GroupInfo, name groupNamer) *wuphfpb.ChatCommunity {
	parent := parentOf(info)
	if parent.IsEmpty() {
		return &wuphfpb.ChatCommunity{}
	}
	return &wuphfpb.ChatCommunity{ChatId: parent.String(), Title: name(parent)}
}

// parentOf is the community a group is linked to; empty for a group in none
// and for a community itself.
func parentOf(info *types.GroupInfo) types.JID {
	if info == nil || info.IsParent {
		return types.JID{}
	}
	return info.LinkedParentJID
}

// communityChange is the community a group notification links the group to
// or unlinks it from: nil when it changed no community of this group.
func communityChange(evt *events.GroupInfo) *wuphfpb.ChatCommunity {
	switch {
	case evt.Link != nil && evt.Link.Type == types.GroupLinkChangeTypeParent:
		return &wuphfpb.ChatCommunity{ChatId: evt.Link.Group.JID.String(), Title: evt.Link.Group.Name}
	case evt.Unlink != nil && evt.Unlink.Type == types.GroupLinkChangeTypeParent:
		return &wuphfpb.ChatCommunity{}
	default:
		return nil
	}
}

// groupNames is a groupNamer over a list of groups (the joined groups).
func groupNames(groups []*types.GroupInfo) groupNamer {
	names := make(map[types.JID]string, len(groups))
	for _, group := range groups {
		names[group.JID] = group.Name
	}
	return func(group types.JID) string { return names[group] }
}
