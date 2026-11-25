//! The wording of system notes: the info card a new mirror topic starts with,
//! and the notes a chat profile change posts. Pure composition, and the only
//! place the wording lives.

use crate::profile::ProfileChanges;
use wuphf_model::{Attachment, ChatProfile, ConnectorId, ParticipantChange, ParticipantChangeKind};

/// One system note: text, and the photo it shows (an avatar).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Note {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

impl Note {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }
}

const DAY: u32 = 86_400;
const HOUR: u32 = 3_600;

/// A disappearing-messages timer as WhatsApp names it: "24 hours", "7 days",
/// "90 days". A whole number of days above one is days; else a whole number of
/// hours is hours (so one day is "24 hours"); else seconds. 0 is "off".
pub(crate) fn timer_text(seconds: u32) -> String {
    let plural = |count: u32, unit: &str| {
        if count == 1 {
            format!("1 {unit}")
        } else {
            format!("{count} {unit}s")
        }
    };
    match seconds {
        0 => "off".to_owned(),
        _ if seconds.is_multiple_of(DAY) && seconds > DAY => plural(seconds / DAY, "day"),
        _ if seconds.is_multiple_of(HOUR) => plural(seconds / HOUR, "hour"),
        _ => plural(seconds, "second"),
    }
}

fn timer_line(seconds: u32) -> String {
    format!("⏳ Disappearing messages: {}", timer_text(seconds))
}

/// The info card: the title line, the source chat's connector (the same chat
/// can be on two accounts of one platform), then (each only when known) the
/// community it belongs to, the description, the timer and the participant
/// count; the avatar attached.
pub(crate) fn info_card(
    title: &str,
    connector: &ConnectorId,
    profile: Option<&ChatProfile>,
    avatar: Option<Attachment>,
) -> Note {
    let mut lines = vec![format!("ℹ️ {title}"), format!("🔌 {connector}")];
    if let Some(profile) = profile {
        if let Some(community) = &profile.community {
            let name = if community.title.is_empty() {
                &community.chat_id
            } else {
                &community.title
            };
            lines.push(format!("🏘️ Community: {name}"));
        }
        if let Some(description) = profile
            .description
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            lines.push(format!("📝 {description}"));
        }
        if let Some(timer) = profile.disappearing {
            lines.push(timer_line(timer.seconds));
        }
        if let Some(count) = profile.participant_count {
            lines.push(if count == 1 {
                "👥 1 participant".to_owned()
            } else {
                format!("👥 {count} participants")
            });
        }
    }
    Note {
        text: lines.join("\n"),
        attachments: avatar.into_iter().collect(),
    }
}

/// One note per change, in this order: description, avatar, timer, then each
/// membership change. A title change is never a note: it renames the thread.
/// `avatar` is the new photo to attach to the "Photo changed" note.
pub(crate) fn change_notes(changes: &ProfileChanges, avatar: Option<Attachment>) -> Vec<Note> {
    let mut notes = Vec::new();
    if let Some(description) = &changes.description {
        notes.push(Note::text(if description.is_empty() {
            "📝 Description removed".to_owned()
        } else {
            format!("📝 Description changed:\n{description}")
        }));
    }
    if let Some(changed) = &changes.avatar {
        notes.push(if changed.id.is_empty() {
            Note::text("🖼️ Photo removed")
        } else {
            Note {
                text: "🖼️ Photo changed".to_owned(),
                attachments: avatar.into_iter().collect(),
            }
        });
    }
    if let Some(timer) = changes.disappearing {
        notes.push(Note::text(timer_line(timer.seconds)));
    }
    notes.extend(
        changes
            .participants
            .iter()
            .map(|change| Note::text(membership_text(change))),
    );
    notes
}

/// "Alice added Bob" when someone other than the member acted; the plain
/// "Bob joined" when nobody is named or the member acted on themself. A
/// missing name falls back to the id.
fn membership_text(change: &ParticipantChange) -> String {
    let name = change.name.as_deref().unwrap_or(&change.participant);
    let actor = change
        .actor
        .as_deref()
        .filter(|actor| *actor != change.participant)
        .map(|actor| change.actor_name.as_deref().unwrap_or(actor));
    match (change.kind, actor) {
        (ParticipantChangeKind::Joined, Some(actor)) => format!("➕ {actor} added {name}"),
        (ParticipantChangeKind::Left, Some(actor)) => format!("➖ {actor} removed {name}"),
        (ParticipantChangeKind::Promoted, Some(actor)) => {
            format!("⭐ {actor} made {name} an admin")
        }
        (ParticipantChangeKind::Demoted, Some(actor)) => {
            format!("{actor} dismissed {name} as admin")
        }
        (ParticipantChangeKind::Joined, None) => format!("➕ {name} joined"),
        (ParticipantChangeKind::Left, None) => format!("➖ {name} left"),
        (ParticipantChangeKind::Promoted, None) => format!("⭐ {name} is now an admin"),
        (ParticipantChangeKind::Demoted, None) => format!("{name} is no longer an admin"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{
        AttachmentKind, ChatAvatar, ChatCommunity, ConnectorId, DisappearingTimer, MediaId,
    };

    #[test]
    fn timer_text_names_days_hours_and_seconds() {
        for (seconds, text) in [
            (0, "off"),
            (86_400, "24 hours"),
            (604_800, "7 days"),
            (7_776_000, "90 days"),
            (3_600, "1 hour"),
            (7_200, "2 hours"),
            (90_000, "25 hours"),
            (1, "1 second"),
            (90, "90 seconds"),
        ] {
            assert_eq!(timer_text(seconds), text, "{seconds}");
        }
    }

    fn photo() -> Attachment {
        Attachment {
            media_id: MediaId::new(),
            kind: AttachmentKind::Image,
            file_name: Some("avatar.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            size_bytes: Some(3),
            animated: false,
        }
    }

    #[test]
    fn the_info_card_lists_what_is_known() {
        let profile = ChatProfile {
            description: Some("Weekend plans".into()),
            disappearing: Some(DisappearingTimer::new(604_800)),
            participant_count: Some(12),
            community: Some(ChatCommunity {
                chat_id: "9@g.us".into(),
                title: "Neighbours".into(),
            }),
            ..ChatProfile::new(ConnectorId("whatsapp".into()), "1@g.us")
        };
        let avatar = photo();
        let whatsapp = ConnectorId("whatsapp-2".into());
        let card = info_card(
            "[G] Family",
            &whatsapp,
            Some(&profile),
            Some(avatar.clone()),
        );
        assert_eq!(
            card.text,
            "ℹ️ [G] Family\n🔌 whatsapp-2\n🏘️ Community: Neighbours\n📝 Weekend plans\n⏳ Disappearing messages: 7 \
             days\n👥 12 participants"
        );
        assert_eq!(card.attachments, vec![avatar]);

        let bare = info_card("[P] Alice", &whatsapp, None, None);
        assert_eq!(bare.text, "ℹ️ [P] Alice\n🔌 whatsapp-2");
        assert!(bare.attachments.is_empty());
    }

    #[test]
    fn change_notes_follow_the_fixed_order_and_wording() {
        let avatar = photo();
        let changes = ProfileChanges {
            description: Some("New plans".into()),
            avatar: Some(ChatAvatar {
                id: "pic-2".into(),
                media_id: Some(avatar.media_id),
                hash: None,
            }),
            disappearing: Some(DisappearingTimer::new(0)),
            participants: vec![change(ParticipantChangeKind::Left, "b", Some("Bob"), None)],
            ..ProfileChanges::default()
        };
        let notes = change_notes(&changes, Some(avatar.clone()));
        let texts: Vec<_> = notes.iter().map(|note| note.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "📝 Description changed:\nNew plans",
                "🖼️ Photo changed",
                "⏳ Disappearing messages: off",
                "➖ Bob left",
            ]
        );
        assert_eq!(notes[1].attachments, vec![avatar]);

        let removed = ProfileChanges {
            description: Some(String::new()),
            avatar: Some(ChatAvatar {
                id: String::new(),
                media_id: None,
                hash: None,
            }),
            ..ProfileChanges::default()
        };
        let texts: Vec<_> = change_notes(&removed, None)
            .into_iter()
            .map(|note| note.text)
            .collect();
        assert_eq!(texts, ["📝 Description removed", "🖼️ Photo removed"]);
    }

    fn change(
        kind: ParticipantChangeKind,
        participant: &str,
        name: Option<&str>,
        actor: Option<(&str, Option<&str>)>,
    ) -> ParticipantChange {
        ParticipantChange {
            kind,
            participant: participant.into(),
            name: name.map(Into::into),
            actor: actor.map(|(id, _)| id.into()),
            actor_name: actor.and_then(|(_, name)| name.map(Into::into)),
        }
    }

    #[test]
    fn membership_notes_name_the_actor() {
        use ParticipantChangeKind::*;
        let alice = Some(("a", Some("Alice")));
        let cases = [
            (
                change(Joined, "b", Some("Bob"), alice),
                "➕ Alice added Bob",
            ),
            (
                change(Left, "b", Some("Bob"), alice),
                "➖ Alice removed Bob",
            ),
            (
                change(Promoted, "b", Some("Bob"), alice),
                "⭐ Alice made Bob an admin",
            ),
            (
                change(Demoted, "b", Some("Bob"), alice),
                "Alice dismissed Bob as admin",
            ),
            (change(Joined, "b", Some("Bob"), None), "➕ Bob joined"),
            (change(Left, "b", Some("Bob"), None), "➖ Bob left"),
            (
                change(Promoted, "b", Some("Bob"), None),
                "⭐ Bob is now an admin",
            ),
            (
                change(Demoted, "b", Some("Bob"), None),
                "Bob is no longer an admin",
            ),
            // The member acting on themself reads as the plain wording.
            (
                change(Left, "b", Some("Bob"), Some(("b", Some("Bob")))),
                "➖ Bob left",
            ),
            (
                change(Joined, "b", Some("Bob"), Some(("b", None))),
                "➕ Bob joined",
            ),
            // Missing names fall back to the ids.
            (change(Joined, "b", None, Some(("a", None))), "➕ a added b"),
            (change(Left, "b", None, None), "➖ b left"),
        ];
        for (change, text) in cases {
            assert_eq!(membership_text(&change), text, "{change:?}");
        }
    }
}
