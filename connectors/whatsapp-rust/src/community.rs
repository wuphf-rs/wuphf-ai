//! WhatsApp communities (plan 085): a group in a community is titled
//! `[G] Community › Group`, and the core is told which community it belongs
//! to, so a rule or `receive_only` naming the community covers it. Like the
//! Go connector's `community.go`.

use whatsapp_rust::wacore::stanza::groups::GroupNotificationAction;
use whatsapp_rust::{GroupMetadata, Jid};
use wuphf_proto::connector as pb;

use crate::names::tagged;
use crate::normalize::bare_id;

/// Joins a community's name and its group's in a title.
const SEPARATOR: &str = " › ";

/// The link type naming the community a group belongs to.
const PARENT_LINK: &str = "parent_group";

/// A group's plain name inside its community: "Community › Group". An
/// unknown (or empty) community name leaves the group's.
pub(crate) fn community_name(community: Option<&str>, group: &str) -> String {
    match community.filter(|name| !name.is_empty()) {
        Some(community) => format!("{community}{SEPARATOR}{group}"),
        None => group.to_owned(),
    }
}

/// A group's display title: "[G] Group", or "[G] Community › Group".
pub(crate) fn group_title(group: &str, community: Option<&str>) -> Option<String> {
    tagged("G", &community_name(community, group))
}

/// The community a group is linked to; none for a group in none and for a
/// community itself.
pub(crate) fn parent_of(metadata: &GroupMetadata) -> Option<&Jid> {
    if metadata.is_parent_group {
        return None;
    }
    metadata.parent_group_jid.as_ref()
}

/// The community as the core is told it: the parent's id and name, or an
/// empty one for a group in none.
pub(crate) fn reported(parent: Option<(&Jid, &str)>) -> pb::ChatCommunity {
    match parent {
        Some((jid, title)) => pb::ChatCommunity {
            chat_id: bare_id(jid),
            title: title.to_owned(),
        },
        None => pb::ChatCommunity::default(),
    }
}

/// Whether a group notification links the group to a community or unlinks
/// it from one (not a sub-group linked to a community, seen from it).
pub(crate) fn changes_community(action: &GroupNotificationAction) -> bool {
    match action {
        GroupNotificationAction::Link { link_type, .. } => link_type == PARENT_LINK,
        GroupNotificationAction::Unlink { unlink_type, .. } => unlink_type == PARENT_LINK,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(parent: Option<&str>, is_parent: bool) -> GroupMetadata {
        GroupMetadata {
            subject: "Garden".into(),
            parent_group_jid: parent.map(|jid| jid.parse().unwrap()),
            is_parent_group: is_parent,
            ..GroupMetadata::default()
        }
    }

    #[test]
    fn a_group_in_a_community_is_titled_under_it() {
        assert_eq!(
            group_title("Garden", Some("Neighbours")).as_deref(),
            Some("[G] Neighbours › Garden")
        );
        assert_eq!(group_title("Garden", None).as_deref(), Some("[G] Garden"));
        assert_eq!(
            group_title("Garden", Some("")).as_deref(),
            Some("[G] Garden")
        );
    }

    #[test]
    fn only_a_member_group_has_a_community() {
        assert_eq!(
            parent_of(&metadata(Some("9@g.us"), false))
                .map(bare_id)
                .as_deref(),
            Some("9@g.us")
        );
        assert!(parent_of(&metadata(None, false)).is_none());
        assert!(parent_of(&metadata(Some("9@g.us"), true)).is_none());
    }

    #[test]
    fn the_core_is_told_the_community_or_none() {
        let parent: Jid = "9@g.us".parse().unwrap();
        let community = reported(Some((&parent, "Neighbours")));
        assert_eq!(
            (community.chat_id.as_str(), community.title.as_str()),
            ("9@g.us", "Neighbours")
        );
        assert!(reported(None).chat_id.is_empty());
    }
}
