//! Chat events: what WhatsApp reports about chats rather than messages (group
//! notifications, picture changes, contact syncs, history syncs), mapped to
//! the core's chat updates, plus the one snapshot a chat gets the first time
//! this process sees it. The core decides what becomes a note; this module
//! only reports what changed.
//!
//! Reports are emitted from event handlers and the normalizer only, never from
//! the sender: the session loop that runs the sender is the one draining the
//! input channel.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use diesel::QueryResult;
use diesel::sqlite::SqliteConnection;
use tokio::sync::mpsc;
use tracing::warn;
use whatsapp_rust::chrono::{DateTime, Utc};
use whatsapp_rust::client::Client;
use whatsapp_rust::types::events::{ContactUpdate, Event, GroupUpdate, PictureUpdate};
use whatsapp_rust::wacore::stanza::groups::{GroupNotificationAction, GroupParticipantInfo};
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Jid, Server};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::participant_change::Kind;

use crate::avatars::{self, Avatars};
use crate::chat_state::{history_state, now_ms, state_report};
use crate::contacts;
use crate::disappearing::{
    CHAT_UPDATE_BATCH, chat_update_frames, group_action_update, history_updates,
};
use crate::groups::GroupFacts;
use crate::identity::Identity;
use crate::names::{ChatTitles, Person, dm_title, number_of, tagged};
use crate::normalize::bare_id;

/// Handles group notifications, picture changes, contact syncs and history
/// syncs. Cheap to clone: every handle shares the one frame sink, namer and
/// avatar uploader.
#[derive(Clone)]
pub struct ChatEvents {
    inputs: mpsc::Sender<pb::ClientFrame>,
    titles: Arc<ChatTitles>,
    avatars: Arc<Avatars>,
    identity: Arc<Identity>,
}

impl ChatEvents {
    pub fn new(
        inputs: mpsc::Sender<pb::ClientFrame>,
        titles: Arc<ChatTitles>,
        avatars: Arc<Avatars>,
        identity: Arc<Identity>,
    ) -> Self {
        Self {
            inputs,
            titles,
            avatars,
            identity,
        }
    }

    pub async fn handle(&self, event: Arc<Event>, client: Arc<Client>) {
        let (updates, snapshot) = match &*event {
            Event::GroupUpdate(update) => {
                // The subject, description, members or timer may have changed.
                self.titles.groups().forget(&update.group_jid);
                (self.group_update(&client, update).await, false)
            }
            Event::PictureUpdate(update) => (self.picture_update(&client, update).await, false),
            Event::ContactUpdate(update) => (
                self.contact_update(&client, update).await,
                update.from_full_sync,
            ),
            // History sync frames describe current state: snapshots.
            Event::HistorySync(_) => (self.history_sync(Arc::clone(&event)).await, true),
            Event::PinUpdate(_) | Event::MuteUpdate(_) | Event::ArchiveUpdate(_) => {
                let Some(report) = state_report(&event, now_ms()) else {
                    return;
                };
                let update = pb::ChatUpdate {
                    chat_id: bare_id(&report.jid),
                    ..report.update
                };
                (vec![update], report.snapshot)
            }
            _ => return,
        };
        self.send(&client, updates, snapshot).await;
    }

    /// Every update goes out under its chat's canonical id, once per chat.
    async fn send(&self, client: &Client, updates: Vec<pb::ChatUpdate>, snapshot: bool) {
        let updates = self.identity.canonical_updates(client, updates).await;
        for frame in chat_update_frames(updates, CHAT_UPDATE_BATCH, snapshot) {
            if self.inputs.send(frame).await.is_err() {
                warn!("core session input closed; dropping WhatsApp chat updates");
                return;
            }
        }
    }

    /// A group notification's update, with every member and the actor named
    /// like message senders.
    async fn group_update(&self, client: &Client, update: &GroupUpdate) -> Vec<pb::ChatUpdate> {
        let actor = actor_jid(
            update.is_lid_addressing_mode,
            update.participant.as_ref(),
            update.participant_pn.as_ref(),
        );
        let mut names = HashMap::new();
        for member in members(&update.action) {
            let person = self
                .titles
                .person(client, &member.jid, member.phone_number.as_ref())
                .await;
            names.insert(
                bare_id(&member.jid),
                member_name(&person, member.display_name.as_deref()),
            );
        }
        if let Some(actor) = actor
            && !names.contains_key(&bare_id(actor))
        {
            let person = self
                .titles
                .person(client, actor, update.participant_pn.as_ref())
                .await;
            names.insert(bare_id(actor), member_name(&person, None));
        }
        group_report(
            &update.group_jid,
            update.timestamp,
            &update.action,
            actor,
            &names,
        )
        .into_iter()
        .collect()
    }

    /// A chat's new picture (uploaded) or its removal.
    async fn picture_update(&self, client: &Client, update: &PictureUpdate) -> Vec<pb::ChatUpdate> {
        let jid = update.jid.to_non_ad();
        if !is_chat(&jid) {
            return Vec::new();
        }
        let avatar = if update.removed {
            avatars::removed()
        } else {
            match self.avatars.fetch(client, &jid).await {
                Some(avatar) => avatar,
                None => return Vec::new(),
            }
        };
        vec![pb::ChatUpdate {
            chat_id: bare_id(&jid),
            avatar: Some(avatar),
            ..Default::default()
        }]
    }

    /// A contact sync: store the saved names, then retitle the person's DM so
    /// a saved-name change renames its thread.
    async fn contact_update(&self, client: &Client, update: &ContactUpdate) -> Vec<pb::ChatUpdate> {
        let jid = update.jid.to_non_ad();
        if !matches!(jid.server, Server::Pn | Server::Lid) {
            return Vec::new();
        }
        self.titles
            .contacts()
            .upsert_contact_name(
                contact_forms(&jid, &update.action),
                update.action.full_name.as_deref(),
                update.action.first_name.as_deref(),
            )
            .await;
        let phone = update.action.pn_jid.as_deref().and_then(user_jid);
        let person = self.titles.person(client, &jid, phone.as_ref()).await;
        let Some(title) = dm_title(false, &person.number, &person.names) else {
            return Vec::new();
        };
        vec![pb::ChatUpdate {
            chat_id: bare_id(&jid),
            title: Some(title),
            ..Default::default()
        }]
    }

    /// A history sync's push names, then every conversation's timer and title.
    async fn history_sync(&self, event: Arc<Event>) -> Vec<pb::ChatUpdate> {
        // Decoding a history sync is CPU-heavy; keep it off the runtime and
        // outside the database permit. The decoded sync is cached in the event.
        let decoding = Arc::clone(&event);
        let decoded = tokio::task::spawn_blocking(move || match &*decoding {
            Event::HistorySync(sync) => sync.get().is_some(),
            _ => false,
        })
        .await;
        match decoded {
            Ok(true) => {}
            Ok(false) => {
                warn!("could not decode a WhatsApp history sync; its chat updates are skipped");
                return Vec::new();
            }
            Err(error) => {
                warn!(%error, "decoding a WhatsApp history sync failed");
                return Vec::new();
            }
        }
        let at = contacts::now();
        let report = self
            .titles
            .contacts()
            .run(move |conn| match &*event {
                Event::HistorySync(sync) => match sync.get() {
                    Some(sync) => history_report(conn, sync, at),
                    None => Ok(Vec::new()),
                },
                _ => Ok(Vec::new()),
            })
            .await;
        report.unwrap_or_else(|error| {
            warn!(%error, "could not map a WhatsApp history sync");
            Vec::new()
        })
    }
}

/// A DM or group chat; status, broadcasts and newsletters are not chats the
/// core mirrors.
fn is_chat(jid: &Jid) -> bool {
    matches!(jid.server, Server::Pn | Server::Lid | Server::Group)
}

/// A phone or LID JID from a string the platform sent, or `None`.
fn user_jid(id: &str) -> Option<Jid> {
    let jid: Jid = id.parse().ok()?;
    matches!(jid.server, Server::Pn | Server::Lid).then(|| jid.to_non_ad())
}

/// The forms a contact sync names a person by: its JID, and the phone and LID
/// forms the action carries.
pub(crate) fn contact_forms(
    jid: &Jid,
    action: &wa::sync_action_value::ContactAction,
) -> Vec<String> {
    let mut forms = vec![bare_id(jid)];
    for other in [&action.pn_jid, &action.lid_jid].into_iter().flatten() {
        if let Some(other) = user_jid(other).map(|jid| bare_id(&jid))
            && !forms.contains(&other)
        {
            forms.push(other);
        }
    }
    forms
}

/// The members a membership notification names.
fn members(action: &GroupNotificationAction) -> &[GroupParticipantInfo] {
    match action {
        GroupNotificationAction::Add { participants, .. }
        | GroupNotificationAction::Remove { participants, .. }
        | GroupNotificationAction::Promote { participants }
        | GroupNotificationAction::Demote { participants } => participants.as_slice(),
        _ => &[],
    }
}

/// Who made a group change, in the members' address form: a LID-addressed
/// group names members by LID, so the actor is `participant`; otherwise the
/// phone form `participant_pn` when present, else `participant`.
fn actor_jid<'a>(
    is_lid_addressing_mode: bool,
    participant: Option<&'a Jid>,
    participant_pn: Option<&'a Jid>,
) -> Option<&'a Jid> {
    if is_lid_addressing_mode {
        participant
    } else {
        participant_pn.or(participant)
    }
}

/// A member's display name: their contact name when one is known, else the
/// label the server sent with the notification, else the number.
fn member_name(person: &Person, display_name: Option<&str>) -> String {
    if person.names.is_named() {
        return person.name();
    }
    match display_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(label) => label.to_owned(),
        None => person.number.clone(),
    }
}

/// What a group notification changes: the title (subject), the description,
/// the timer, or membership. `names` maps canonical ids (members and actor)
/// to display names. `None` for anything else.
fn group_report(
    group: &Jid,
    at: DateTime<Utc>,
    action: &GroupNotificationAction,
    actor: Option<&Jid>,
    names: &HashMap<String, String>,
) -> Option<pb::ChatUpdate> {
    let chat_id = bare_id(group);
    let kind = match action {
        GroupNotificationAction::Ephemeral { .. } => {
            return group_action_update(group, at, action);
        }
        GroupNotificationAction::Subject { subject, .. } => {
            return Some(pb::ChatUpdate {
                chat_id,
                title: Some(tagged("G", subject)?),
                ..Default::default()
            });
        }
        GroupNotificationAction::Description { description, .. } => {
            return Some(pb::ChatUpdate {
                chat_id,
                description: Some(description.clone().unwrap_or_default()),
                ..Default::default()
            });
        }
        GroupNotificationAction::Add { .. } => Kind::Joined,
        GroupNotificationAction::Remove { .. } => Kind::Left,
        GroupNotificationAction::Promote { .. } => Kind::Promoted,
        GroupNotificationAction::Demote { .. } => Kind::Demoted,
        _ => return None,
    };
    let actor = actor.map(bare_id);
    let actor_name = actor.as_ref().and_then(|actor| names.get(actor).cloned());
    let participant_changes = members(action)
        .iter()
        .map(|member| {
            let participant = bare_id(&member.jid);
            pb::ParticipantChange {
                kind: kind.into(),
                name: names.get(&participant).cloned(),
                participant,
                actor: actor.clone(),
                actor_name: actor_name.clone(),
            }
        })
        .collect::<Vec<_>>();
    (!participant_changes.is_empty()).then(|| pb::ChatUpdate {
        chat_id,
        participant_changes,
        ..Default::default()
    })
}

/// A history sync, in order: store its push names, then map every
/// conversation's timer, pin / mute / archive state and title (groups by
/// their synced name, DMs from the contacts table, which now holds the sync's
/// push names). `at` is now, in Unix seconds (a mute that ended reads as
/// none).
pub fn history_report(
    conn: &mut SqliteConnection,
    sync: &wa::HistorySync,
    at: i64,
) -> QueryResult<Vec<pb::ChatUpdate>> {
    let push_names = sync
        .pushnames
        .iter()
        .filter_map(|entry| {
            let jid = user_jid(entry.id.as_deref()?)?;
            Some((bare_id(&jid), entry.pushname.clone()?))
        })
        .collect::<Vec<_>>();
    contacts::store_push_names(conn, &push_names, at)?;
    let now_ms = at.saturating_mul(1000);
    Ok(history_updates(sync, |conversation, chat| {
        let title = history_title(conn, conversation, chat).unwrap_or_else(|error| {
            warn!(%error, chat = %chat, "could not read a WhatsApp contact name");
            None
        });
        pb::ChatUpdate {
            title,
            ..history_state(conversation, now_ms)
        }
    }))
}

/// A synced conversation's title: a group's name, or a DM's contact name.
fn history_title(
    conn: &mut SqliteConnection,
    conversation: &wa::Conversation,
    chat: &Jid,
) -> QueryResult<Option<String>> {
    match chat.server {
        Server::Group => Ok(conversation
            .name
            .as_deref()
            .and_then(|name| tagged("G", name))),
        Server::Pn | Server::Lid => {
            let mut forms = vec![bare_id(chat)];
            for other in [&conversation.pn_jid, &conversation.lid_jid]
                .into_iter()
                .flatten()
            {
                if let Some(other) = user_jid(other).map(|jid| bare_id(&jid))
                    && !forms.contains(&other)
                {
                    forms.push(other);
                }
            }
            let names = contacts::lookup(conn, &forms)?;
            let phone = conversation.pn_jid.as_deref().and_then(user_jid);
            let number = number_of(chat, None, phone.as_ref());
            // A title only from real names: a bare number would rename a
            // thread the core already named better.
            Ok(dm_title(true, &number, &names))
        }
        _ => Ok(None),
    }
}

/// The chats this process has reported a snapshot for.
#[derive(Default)]
pub struct Sightings {
    seen: Mutex<HashSet<String>>,
}

impl Sightings {
    /// True the first time `chat` is seen by this process.
    pub fn first(&self, chat: &str) -> bool {
        self.seen.lock().unwrap().insert(chat.to_owned())
    }
}

/// The snapshot a chat's first sighting reports, under each of `forms`: the
/// title the message carries, a group's description and member count, and
/// the picture. Nothing when nothing is known.
pub fn sighting_updates(
    forms: Vec<String>,
    title: Option<String>,
    group: Option<&GroupFacts>,
    avatar: Option<pb::ChatAvatar>,
) -> Vec<pb::ChatUpdate> {
    if title.is_none() && group.is_none() && avatar.is_none() {
        return Vec::new();
    }
    forms
        .into_iter()
        .map(|chat_id| pb::ChatUpdate {
            chat_id,
            title: title.clone(),
            description: group.map(|facts| facts.description.clone()),
            participant_count: group.map(|facts| facts.participant_count),
            avatar: avatar.clone(),
            ..Default::default()
        })
        .collect()
}

/// The LID and phone user parts of a person, when the client knows the
/// mapping.
pub async fn lid_pn(client: &Client, jid: &Jid) -> Option<(String, String)> {
    if !matches!(jid.server, Server::Pn | Server::Lid) {
        return None;
    }
    let entry = client.get_lid_pn_entry(jid).await.ok()??;
    Some((entry.lid.to_string(), entry.phone_number.to_string()))
}

/// Every form of a chat, for the connector's own contacts-table lookups (never
/// the wire): its bare id, plus for a DM the other (phone or LID) form under a
/// known `(lid, phone)` mapping.
pub fn forms_of(jid: &Jid, mapping: Option<(&str, &str)>) -> Vec<String> {
    let mut forms = vec![bare_id(jid)];
    let other = match (jid.server, mapping) {
        (Server::Lid, Some((_, pn))) => Jid::pn(pn),
        (Server::Pn, Some((lid, _))) => Jid::lid(lid),
        _ => return forms,
    };
    let other = bare_id(&other);
    if !forms.contains(&other) {
        forms.push(other);
    }
    forms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::tests::memory;

    fn jid(id: &str) -> Jid {
        id.parse().unwrap()
    }

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn member(id: &str, phone: Option<&str>, label: Option<&str>) -> GroupParticipantInfo {
        GroupParticipantInfo {
            jid: jid(id),
            phone_number: phone.map(jid),
            display_name: label.map(str::to_owned),
            r#type: None,
            lid: None,
            username: None,
            join_time: None,
        }
    }

    fn names(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
            .collect()
    }

    fn report(action: GroupNotificationAction, actor: Option<&Jid>) -> Option<pb::ChatUpdate> {
        group_report(
            &jid("123@g.us"),
            at(),
            &action,
            actor,
            &names(&[
                ("1555@s.whatsapp.net", "Bob (1555)"),
                ("1777@s.whatsapp.net", "Alice Smith"),
                ("8888@lid", "Bob (1555)"),
                ("9999@lid", "Alice Smith"),
            ]),
        )
    }

    fn change(
        kind: Kind,
        participant: &str,
        name: Option<&str>,
        actor: Option<(&str, &str)>,
    ) -> pb::ParticipantChange {
        pb::ParticipantChange {
            kind: kind.into(),
            participant: participant.into(),
            name: name.map(str::to_owned),
            actor: actor.map(|(id, _)| id.to_owned()),
            actor_name: actor.map(|(_, name)| name.to_owned()),
        }
    }

    #[test]
    fn a_subject_change_retitles_the_group() {
        let update = report(
            GroupNotificationAction::Subject {
                subject: "Family".into(),
                subject_owner: None,
                subject_time: None,
            },
            None,
        );
        assert_eq!(
            update,
            Some(pb::ChatUpdate {
                chat_id: "123@g.us".into(),
                title: Some("[G] Family".into()),
                ..Default::default()
            })
        );
        let blank = report(
            GroupNotificationAction::Subject {
                subject: " ".into(),
                subject_owner: None,
                subject_time: None,
            },
            None,
        );
        assert_eq!(blank, None);
    }

    #[test]
    fn a_description_change_or_removal_is_reported() {
        for (description, reported) in [(Some("Our family".to_owned()), "Our family"), (None, "")] {
            let update = report(
                GroupNotificationAction::Description {
                    id: "d1".into(),
                    description,
                },
                None,
            );
            assert_eq!(
                update,
                Some(pb::ChatUpdate {
                    chat_id: "123@g.us".into(),
                    description: Some(reported.into()),
                    ..Default::default()
                })
            );
        }
    }

    #[test]
    fn the_timer_is_still_reported() {
        let update = report(
            GroupNotificationAction::Ephemeral {
                expiration: 86_400,
                trigger: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(update.disappearing.map(|timer| timer.seconds), Some(86_400));
    }

    #[test]
    fn each_membership_action_reports_one_change_per_member() {
        let alice = jid("1777@s.whatsapp.net");
        let members = || {
            vec![
                member("1555@s.whatsapp.net", None, None),
                member("1666@s.whatsapp.net", None, None),
            ]
        };
        let actions = [
            (
                GroupNotificationAction::Add {
                    participants: members(),
                    reason: None,
                },
                Kind::Joined,
            ),
            (
                GroupNotificationAction::Remove {
                    participants: members(),
                    reason: None,
                },
                Kind::Left,
            ),
            (
                GroupNotificationAction::Promote {
                    participants: members(),
                },
                Kind::Promoted,
            ),
            (
                GroupNotificationAction::Demote {
                    participants: members(),
                },
                Kind::Demoted,
            ),
        ];
        for (action, kind) in actions {
            let with_actor = report(action.clone(), Some(&alice)).unwrap();
            assert_eq!(
                with_actor.participant_changes,
                [
                    change(
                        kind,
                        "1555@s.whatsapp.net",
                        Some("Bob (1555)"),
                        Some(("1777@s.whatsapp.net", "Alice Smith"))
                    ),
                    // A member nobody named keeps no name; the core shows the id.
                    change(
                        kind,
                        "1666@s.whatsapp.net",
                        None,
                        Some(("1777@s.whatsapp.net", "Alice Smith"))
                    ),
                ]
            );
            assert_eq!(with_actor.chat_id, "123@g.us");
            assert_eq!(with_actor.title, None);
            assert_eq!(with_actor.participant_count, None);

            let without_actor = report(action, None).unwrap();
            assert_eq!(
                without_actor.participant_changes[0],
                change(kind, "1555@s.whatsapp.net", Some("Bob (1555)"), None)
            );
        }
    }

    #[test]
    fn the_actor_is_taken_in_the_members_address_form() {
        let lid = jid("9999@lid");
        let pn = jid("1777@s.whatsapp.net");
        // A LID-addressed group names members by LID.
        assert_eq!(actor_jid(true, Some(&lid), Some(&pn)), Some(&lid));
        // A phone-addressed group: the phone form when present.
        assert_eq!(actor_jid(false, Some(&lid), Some(&pn)), Some(&pn));
        assert_eq!(actor_jid(false, Some(&pn), None), Some(&pn));
        assert_eq!(actor_jid(false, None, None), None);

        let update = report(
            GroupNotificationAction::Remove {
                participants: vec![member("8888@lid", Some("1555@s.whatsapp.net"), None)],
                reason: None,
            },
            actor_jid(true, Some(&lid), Some(&pn)),
        )
        .unwrap();
        assert_eq!(
            update.participant_changes,
            [change(
                Kind::Left,
                "8888@lid",
                Some("Bob (1555)"),
                Some(("9999@lid", "Alice Smith"))
            )]
        );
    }

    #[test]
    fn a_self_leave_reports_the_member_as_the_actor() {
        let bob = jid("8888@lid");
        let update = report(
            GroupNotificationAction::Remove {
                participants: vec![member("8888@lid", None, None)],
                reason: None,
            },
            actor_jid(true, Some(&bob), Some(&jid("1555@s.whatsapp.net"))),
        )
        .unwrap();
        let change = &update.participant_changes[0];
        assert_eq!(change.actor.as_deref(), Some(change.participant.as_str()));
        assert_eq!(change.actor_name, change.name);
    }

    #[test]
    fn other_group_actions_report_nothing() {
        assert_eq!(report(GroupNotificationAction::Unlocked, None), None);
        assert_eq!(
            report(
                GroupNotificationAction::Add {
                    participants: vec![],
                    reason: None
                },
                None
            ),
            None
        );
    }

    #[test]
    fn a_member_is_named_by_contact_then_label_then_number() {
        let known = Person {
            number: "1555".into(),
            names: contacts::ContactNames {
                push_name: Some("Bob".into()),
                ..Default::default()
            },
        };
        assert_eq!(member_name(&known, Some("+1∙∙∙55")), "Bob (1555)");
        let unknown = Person {
            number: "1555".into(),
            names: Default::default(),
        };
        assert_eq!(member_name(&unknown, Some("+1∙∙∙55")), "+1∙∙∙55");
        assert_eq!(member_name(&unknown, Some(" ")), "1555");
        assert_eq!(member_name(&unknown, None), "1555");
    }

    #[test]
    fn a_contact_sync_names_every_form() {
        let action = wa::sync_action_value::ContactAction {
            full_name: Some("Alice Smith".into()),
            pn_jid: Some("1777@s.whatsapp.net".into()),
            lid_jid: Some("9999@lid".into()),
            ..Default::default()
        };
        assert_eq!(
            contact_forms(&jid("9999@lid"), &action),
            ["9999@lid", "1777@s.whatsapp.net"]
        );
    }

    #[test]
    fn the_other_form_comes_from_the_mapping() {
        assert_eq!(
            forms_of(&jid("8888@lid"), Some(("8888", "1555"))),
            ["8888@lid", "1555@s.whatsapp.net"]
        );
        assert_eq!(
            forms_of(&jid("1555@s.whatsapp.net"), Some(("8888", "1555"))),
            ["1555@s.whatsapp.net", "8888@lid"]
        );
        assert_eq!(forms_of(&jid("8888@lid"), None), ["8888@lid"]);
        assert_eq!(
            forms_of(&jid("123@g.us"), Some(("8888", "1555"))),
            ["123@g.us"]
        );
    }

    fn pushname(id: &str, name: &str) -> wa::Pushname {
        wa::Pushname {
            id: Some(id.into()),
            pushname: Some(name.into()),
        }
    }

    #[test]
    fn a_history_sync_stores_push_names_before_it_titles_chats() {
        let mut conn = memory();
        let sync = wa::HistorySync {
            pushnames: vec![
                pushname("1555@s.whatsapp.net", "Alice"),
                pushname("8888@lid", "Bob"),
            ],
            conversations: vec![
                wa::Conversation {
                    id: "1@g.us".into(),
                    name: Some("Family".into()),
                    ..Default::default()
                },
                wa::Conversation {
                    id: "1555@s.whatsapp.net".into(),
                    ..Default::default()
                },
                wa::Conversation {
                    id: "8888@lid".into(),
                    pn_jid: Some("1666@s.whatsapp.net".into()),
                    ..Default::default()
                },
                // Nobody named: no title rather than a bare number.
                wa::Conversation {
                    id: "1999@s.whatsapp.net".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let titles = history_report(&mut conn, &sync, 1)
            .unwrap()
            .into_iter()
            .map(|update| (update.chat_id, update.title))
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            [
                ("1@g.us".to_owned(), Some("[G] Family".to_owned())),
                (
                    "1555@s.whatsapp.net".to_owned(),
                    Some("[P] Alice (1555)".to_owned())
                ),
                ("8888@lid".to_owned(), Some("[P] Bob (1666)".to_owned())),
                ("1999@s.whatsapp.net".to_owned(), None),
            ]
        );
        // The frames say the report is a snapshot.
        let frames = chat_update_frames(history_report(&mut conn, &sync, 2).unwrap(), 500, true);
        assert!(frames.iter().all(|frame| matches!(
            &frame.body,
            Some(pb::client_frame::Body::ChatUpdate(event)) if event.snapshot
        )));
    }

    #[test]
    fn a_history_sync_reports_chat_state() {
        let mut conn = memory();
        let sync = wa::HistorySync {
            conversations: vec![wa::Conversation {
                id: "8888@lid".into(),
                pn_jid: Some("1666@s.whatsapp.net".into()),
                pinned: Some(1),
                archived: Some(true),
                // Seconds; still in the future at `at`.
                mute_end_time: Some(1_900_000_000),
                ..Default::default()
            }],
            ..Default::default()
        };
        let updates = history_report(&mut conn, &sync, 1_700_000_000).unwrap();
        assert_eq!(updates.len(), 1);
        for update in updates {
            assert_eq!(update.pinned, Some(true));
            assert_eq!(update.archived, Some(true));
            assert_eq!(
                update.mute,
                Some(pb::MuteState {
                    muted_until: 1_900_000_000
                })
            );
        }
        // The same end has passed by 2031: no mute.
        let updates = history_report(&mut conn, &sync, 1_950_000_000).unwrap();
        assert_eq!(updates[0].mute, Some(pb::MuteState { muted_until: 0 }));
    }

    #[test]
    fn a_first_sighting_reports_one_snapshot_per_form() {
        let sightings = Sightings::default();
        assert!(sightings.first("123@g.us"));
        assert!(!sightings.first("123@g.us"));
        assert!(sightings.first("1555@s.whatsapp.net"));

        let facts = GroupFacts {
            subject: "Family".into(),
            description: "Our family".into(),
            participant_count: 4,
            disappearing_seconds: 0,
        };
        let avatar = pb::ChatAvatar {
            id: "pic-1".into(),
            media_id: Some("m1".into()),
        };
        assert_eq!(
            sighting_updates(
                vec!["123@g.us".into()],
                Some("[G] Family".into()),
                Some(&facts),
                Some(avatar.clone()),
            ),
            [pb::ChatUpdate {
                chat_id: "123@g.us".into(),
                title: Some("[G] Family".into()),
                description: Some("Our family".into()),
                participant_count: Some(4),
                avatar: Some(avatar),
                ..Default::default()
            }]
        );
        let dm = sighting_updates(
            vec!["8888@lid".into(), "1555@s.whatsapp.net".into()],
            Some("[P] Bob (1555)".into()),
            None,
            Some(avatars::removed()),
        );
        assert_eq!(dm.len(), 2);
        assert!(dm.iter().all(|update| update.description.is_none()
            && update.participant_count.is_none()
            && update.participant_changes.is_empty()));
        assert!(sighting_updates(vec!["1@s.whatsapp.net".into()], None, None, None).is_empty());
    }
}
