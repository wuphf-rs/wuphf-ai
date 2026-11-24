//! Health probes (plan 079): whether this account is in a group and may post
//! there (an announce-only group takes an admin), like the Go connector's
//! prober. WhatsApp has no topics. While no session is linked, every chat is
//! unknown.

use whatsapp_rust::{GroupError, GroupMetadata, IqError, Jid, Server};
use wuphf_connector::Prober;
use wuphf_connector::commands::BoxFuture;
use wuphf_proto::connector as pb;

use crate::session::LinkedClient;

pub struct WhatsAppProber {
    linked: LinkedClient,
}

impl WhatsAppProber {
    pub fn new(linked: LinkedClient) -> Self {
        Self { linked }
    }
}

impl Prober for WhatsAppProber {
    fn probe<'a>(&'a self, chat_ids: &'a [String]) -> BoxFuture<'a, Vec<pb::ChatFacts>> {
        Box::pin(async move {
            let Ok(client) = self.linked.get() else {
                return Vec::new();
            };
            let own: Vec<String> = [client.get_pn(), client.get_lid()]
                .into_iter()
                .flatten()
                .map(|jid| jid.user.to_string())
                .collect();
            let mut answers = Vec::with_capacity(chat_ids.len());
            for chat_id in chat_ids {
                let mut facts = pb::ChatFacts {
                    chat_id: chat_id.clone(),
                    forum: fact(false),
                    can_create_topics: fact(false),
                    ..Default::default()
                };
                match chat_id.parse::<Jid>() {
                    Err(error) => facts.detail = error.to_string(),
                    Ok(jid) if jid.server != Server::Group => {
                        // A direct chat is always there to write to.
                        facts.exists = fact(true);
                        facts.member = fact(true);
                        facts.can_send = fact(true);
                    }
                    Ok(jid) => match client.groups().get_metadata(&jid).await {
                        Ok(metadata) => group_facts(&mut facts, &metadata, &own),
                        Err(error) => lookup_failed(&mut facts, &error),
                    },
                }
                answers.push(facts);
            }
            answers
        })
    }
}

fn fact(yes: bool) -> i32 {
    if yes {
        pb::Fact::Yes.into()
    } else {
        pb::Fact::No.into()
    }
}

/// What a group's metadata says of this account (`own`: the users of its
/// phone number and LID).
fn group_facts(facts: &mut pb::ChatFacts, metadata: &GroupMetadata, own: &[String]) {
    let me = metadata.participants.iter().find(|participant| {
        std::iter::once(&participant.jid)
            .chain(participant.phone_number.as_ref())
            .any(|jid| own.iter().any(|user| *user == jid.user.as_str()))
    });
    let member = me.is_some();
    let admin = me.is_some_and(|participant| participant.is_admin());
    facts.exists = fact(true);
    facts.member = fact(member);
    facts.can_send = fact(member && (!metadata.is_announcement || admin));
    if member && metadata.is_announcement && !admin {
        facts.detail = "only admins can send messages".to_owned();
    }
    facts.can_delete_others = fact(admin);
}

/// A failed lookup: the server says not a member (403) or no such group
/// (404); anything else is unknown.
fn lookup_failed(facts: &mut pb::ChatFacts, error: &GroupError) {
    facts.detail = error.to_string();
    if let GroupError::Iq(IqError::ServerError { code, .. }) = error {
        match code {
            403 => {
                facts.exists = fact(true);
                facts.member = fact(false);
                facts.can_send = fact(false);
            }
            404 => facts.exists = fact(false),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::GroupParticipant;

    fn participant(jid: &str, admin: bool) -> GroupParticipant {
        let mut participant = GroupParticipant {
            jid: jid.parse().unwrap(),
            phone_number: None,
            participant_type: Default::default(),
        };
        if admin {
            participant.participant_type = whatsapp_rust::ParticipantType::Admin;
        }
        participant
    }

    fn metadata(announce: bool, participants: Vec<GroupParticipant>) -> GroupMetadata {
        GroupMetadata {
            is_announcement: announce,
            participants,
            ..Default::default()
        }
    }

    fn facts_for(metadata: &GroupMetadata) -> pb::ChatFacts {
        let mut facts = pb::ChatFacts::default();
        group_facts(&mut facts, metadata, &["55".to_owned()]);
        facts
    }

    #[test]
    fn an_announce_only_group_takes_an_admin_to_post() {
        let member = facts_for(&metadata(true, vec![participant("55@lid", false)]));
        assert_eq!(member.member(), pb::Fact::Yes);
        assert_eq!(member.can_send(), pb::Fact::No);
        assert!(!member.detail.is_empty());
        let admin = facts_for(&metadata(true, vec![participant("55@lid", true)]));
        assert_eq!(admin.can_send(), pb::Fact::Yes);
        assert_eq!(admin.can_delete_others(), pb::Fact::Yes);
    }

    #[test]
    fn an_account_not_among_the_participants_is_out() {
        let out = facts_for(&metadata(false, vec![participant("99@lid", false)]));
        assert_eq!(out.member(), pb::Fact::No);
        assert_eq!(out.can_send(), pb::Fact::No);
    }

    #[test]
    fn server_errors_say_gone_or_out() {
        let mut gone = pb::ChatFacts::default();
        lookup_failed(
            &mut gone,
            &GroupError::Iq(IqError::ServerError {
                code: 404,
                text: "item-not-found".into(),
                error_type: None,
                backoff: None,
            }),
        );
        assert_eq!(gone.exists(), pb::Fact::No);
        let mut unknown = pb::ChatFacts::default();
        lookup_failed(&mut unknown, &GroupError::Iq(IqError::Timeout));
        assert_eq!(unknown.exists(), pb::Fact::Unknown);
    }
}
