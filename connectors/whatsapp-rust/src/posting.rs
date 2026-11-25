//! What this account may do in its chats (plan 083; Go `posting.go`), told to
//! the core so it refuses what WhatsApp would not take, or would take wrongly:
//! a message to status@broadcast is a story to every contact. A snapshot goes
//! out each time the session connects; a group goes again when its
//! admin-only setting or this account's admin role changes.

use tracing::warn;
use whatsapp_rust::client::Client;
use whatsapp_rust::wacore::stanza::groups::GroupNotificationAction;
use whatsapp_rust::{GroupMetadata, Jid, NewsletterRole, Server};
use wuphf_proto::connector as pb;

use crate::normalize::bare_id;
use crate::probe::{membership, own_users};

/// WhatsApp's story feed. A send to it is posted as a story to every contact
/// the status privacy allows.
pub(crate) fn is_status_broadcast(jid: &Jid) -> bool {
    jid.server == Server::Broadcast && jid.user == "status"
}

/// A broadcast list (not the story feed): its messages belong in the direct
/// chat with each person (plan 085).
pub(crate) fn is_broadcast_list(jid: &Jid) -> bool {
    jid.server == Server::Broadcast && !is_status_broadcast(jid)
}

/// The story feed's posting: no messages (a story is posted only on
/// purpose), reactions (sent to the poster's DM), and a story is quoted only
/// in its poster's chat.
pub(crate) fn status_posting() -> pb::ChatPosting {
    pb::ChatPosting {
        messages: false,
        reactions: true,
        private_replies: true,
        // No posts: see the sender (plan 084).
        posts: false,
        audience: None,
        post_edits: false,
    }
}

/// What this account may do in a group: anything, unless the group is
/// admin-only (a community's announcement group is) and the account is not
/// an admin there.
pub(crate) fn group_posting(metadata: &GroupMetadata, own: &[String]) -> pb::ChatPosting {
    let (member, admin) = membership(metadata, own);
    pb::ChatPosting {
        messages: !metadata.is_announcement || (member && admin),
        reactions: true,
        private_replies: false,
        posts: false,
        audience: None,
        post_edits: false,
    }
}

/// What this account may do in a channel (plan 085): nothing. A channel post
/// reaches every follower, so it goes only as an explicit post, which this
/// connector does not make yet (Go `newsletterPosting` does); and reactions
/// name a post by a server id the bridge does not keep. The role is kept for
/// when it does.
pub(crate) fn newsletter_posting(_role: Option<&NewsletterRole>) -> pb::ChatPosting {
    pb::ChatPosting {
        messages: false,
        reactions: false,
        private_replies: false,
        posts: false,
        audience: None,
        post_edits: false,
    }
}

/// Whether a group notification may change what this account may do there:
/// the admin-only setting, or this account promoted or demoted.
pub(crate) fn affects_posting(action: &GroupNotificationAction, own: &[String]) -> bool {
    match action {
        GroupNotificationAction::Announce | GroupNotificationAction::NotAnnounce => true,
        GroupNotificationAction::Promote { participants }
        | GroupNotificationAction::Demote { participants } => participants.iter().any(|member| {
            std::iter::once(&member.jid)
                .chain(member.phone_number.as_ref())
                .any(|jid| own.iter().any(|user| *user == jid.user.as_str()))
        }),
        _ => false,
    }
}

/// The posting snapshot: the story feed, every joined group, every followed
/// channel. A list that cannot be read is skipped (logged): the core keeps
/// what it knew.
pub(crate) async fn snapshot(client: &Client) -> Vec<pb::ChatUpdate> {
    let mut updates = vec![pb::ChatUpdate {
        chat_id: "status@broadcast".to_owned(),
        posting: Some(status_posting()),
        ..Default::default()
    }];
    let own = own_users(client);
    match client.groups().get_participating().await {
        Ok(groups) => updates.extend(groups.into_values().map(|group| pb::ChatUpdate {
            chat_id: bare_id(&group.id),
            posting: Some(group_posting(&group, &own)),
            ..Default::default()
        })),
        Err(error) => warn!(%error, "could not list WhatsApp groups for their posting rights"),
    }
    match client.newsletter().list_subscribed().await {
        Ok(channels) => updates.extend(channels.into_iter().map(|channel| pb::ChatUpdate {
            chat_id: bare_id(&channel.jid),
            posting: Some(newsletter_posting(channel.role.as_ref())),
            ..Default::default()
        })),
        Err(error) => warn!(%error, "could not list WhatsApp channels for their posting rights"),
    }
    updates
}

/// A group's posting, read again after a notification that affects it.
pub(crate) async fn group(client: &Client, group: &Jid) -> Option<pb::ChatUpdate> {
    match client.groups().get_metadata(group).await {
        Ok(metadata) => Some(pb::ChatUpdate {
            chat_id: bare_id(group),
            posting: Some(group_posting(&metadata, &own_users(client))),
            ..Default::default()
        }),
        Err(error) => {
            warn!(%error, %group, "could not read a WhatsApp group's posting rights");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::{GroupParticipant, ParticipantType};

    fn metadata(announce: bool, me_admin: Option<bool>) -> GroupMetadata {
        let mut metadata = GroupMetadata {
            is_announcement: announce,
            ..GroupMetadata::default()
        };
        if let Some(admin) = me_admin {
            metadata.participants.push(GroupParticipant {
                jid: "55@lid".parse().unwrap(),
                phone_number: None,
                participant_type: if admin {
                    ParticipantType::Admin
                } else {
                    Default::default()
                },
            });
        }
        metadata
    }

    #[test]
    fn status_takes_reactions_only_and_private_replies() {
        let posting = status_posting();
        assert!(!posting.messages && posting.reactions && posting.private_replies);
        assert!(is_status_broadcast(&"status@broadcast".parse().unwrap()));
        assert!(!is_status_broadcast(&"123@broadcast".parse().unwrap()));
    }

    #[test]
    fn a_broadcast_list_is_not_the_story_feed() {
        assert!(is_broadcast_list(&"1700000000@broadcast".parse().unwrap()));
        assert!(!is_broadcast_list(&"status@broadcast".parse().unwrap()));
        assert!(!is_broadcast_list(&"123@g.us".parse().unwrap()));
    }

    #[test]
    fn an_admin_only_group_takes_an_admin() {
        let own = vec!["55".to_owned()];
        assert!(!group_posting(&metadata(true, Some(false)), &own).messages);
        assert!(group_posting(&metadata(true, Some(true)), &own).messages);
        assert!(group_posting(&metadata(false, Some(false)), &own).messages);
    }

    #[test]
    fn a_channel_is_receive_only_here() {
        for role in [
            Some(&NewsletterRole::Owner),
            Some(&NewsletterRole::Subscriber),
            None,
        ] {
            let posting = newsletter_posting(role);
            assert!(!posting.messages && !posting.reactions && !posting.posts);
        }
    }

    #[test]
    fn only_announce_changes_and_own_role_changes_affect_posting() {
        let own = vec!["55".to_owned()];
        assert!(affects_posting(&GroupNotificationAction::Announce, &own));
        assert!(affects_posting(&GroupNotificationAction::NotAnnounce, &own));
        assert!(!affects_posting(&GroupNotificationAction::Unlocked, &own));
    }
}
