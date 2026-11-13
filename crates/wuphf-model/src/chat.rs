//! What the core knows about a platform chat, apart from routing: its chat
//! profile, and the updates a connector reports about it.

use crate::*;
use serde::{Deserialize, Serialize};

/// A chat's disappearing-messages timer. `seconds` 0 means off.
/// `setting_timestamp` is when it was set (Unix seconds), when known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisappearingTimer {
    pub seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting_timestamp: Option<i64>,
}

impl DisappearingTimer {
    /// A timer of `seconds` whose setting time is unknown.
    pub fn new(seconds: u32) -> Self {
        Self {
            seconds,
            setting_timestamp: None,
        }
    }

    /// A timer of `seconds` set at `setting_timestamp` (Unix seconds).
    pub fn set_at(seconds: u32, setting_timestamp: i64) -> Self {
        Self {
            seconds,
            setting_timestamp: Some(setting_timestamp),
        }
    }
}

/// Everything the core knows about one platform chat on one connector, keyed
/// by its native address. Fields are optional and default to unknown, so a
/// later field needs no migration: stored rows simply lack it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatProfile {
    pub connector_id: ConnectorId,
    pub chat_id: String,
    /// `None` until a timer is learned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<DisappearingTimer>,
    /// The display title, exactly as the connector sends it with messages
    /// (tags included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The group description or chat about text. `Some("")` when known to be
    /// empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<ChatAvatar>,
    /// The member count. The core never stores the member list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_count: Option<u32>,
}

impl ChatProfile {
    /// A profile that knows nothing yet about the chat.
    pub fn new(connector_id: ConnectorId, chat_id: impl Into<String>) -> Self {
        Self {
            connector_id,
            chat_id: chat_id.into(),
            disappearing: None,
            title: None,
            description: None,
            avatar: None,
            participant_count: None,
        }
    }
}

/// The chat's picture as the core stores it. `id` is the platform's picture
/// id; empty means the picture was removed. `media_id` is the picture in the
/// core's media store, and `hash` the core's SHA-256 (hex) of its bytes: equal
/// hashes are the same picture, whatever id the platform gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAvatar {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_id: Option<MediaId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// The chat's picture as a connector reports it: the platform's picture id
/// (empty = removed) and the uploaded bytes, when the connector fetched them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedAvatar {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_id: Option<MediaId>,
}

/// What happened to a chat member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantChangeKind {
    Joined,
    Left,
    Promoted,
    Demoted,
}

/// One membership event. `participant` and `actor` are canonical ids in the
/// same address form, so `actor == participant` means the member acted on
/// themself (a self-join, a self-leave).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipantChange {
    pub kind: ParticipantChangeKind,
    pub participant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Who made the change, when the platform says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_name: Option<String>,
}

/// A connector's report about one chat. `None` fields were not reported.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUpdate {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<DisappearingTimer>,
    /// The display title, exactly as the connector sends it with messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Empty means cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<ReportedAvatar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_count: Option<u32>,
    /// Membership events, in order. Never part of a snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participant_changes: Vec<ParticipantChange>,
}

impl ChatUpdate {
    /// An update about `chat_id` that reports nothing yet.
    pub fn new(chat_id: impl Into<String>) -> Self {
        Self {
            chat_id: chat_id.into(),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_without_a_timer_stores_no_timer_key() {
        let profile = ChatProfile::new(ConnectorId("c".into()), "x");
        let json = serde_json::to_string(&profile).unwrap();
        assert!(!json.contains("disappearing"), "{json}");

        let decoded: ChatProfile = serde_json::from_str(r#"{"connector_id":"c","chat_id":"x"}"#)
            .expect("a profile without a timer decodes");
        assert_eq!(decoded, profile);
        assert_eq!(decoded.disappearing, None);
    }

    #[test]
    fn new_profile_fields_are_absent_when_unknown() {
        let json = serde_json::to_string(&ChatProfile::new(ConnectorId("c".into()), "x")).unwrap();
        assert_eq!(json, r#"{"connector_id":"c","chat_id":"x"}"#);
    }

    #[test]
    fn a_change_without_an_actor_stores_no_actor_keys() {
        let change = ParticipantChange {
            kind: ParticipantChangeKind::Joined,
            participant: "1@s.whatsapp.net".into(),
            name: Some("Bob".into()),
            actor: None,
            actor_name: None,
        };
        assert_eq!(
            serde_json::to_string(&change).unwrap(),
            r#"{"kind":"joined","participant":"1@s.whatsapp.net","name":"Bob"}"#
        );
    }

    #[test]
    fn a_timer_without_a_setting_time_stores_no_timestamp_key() {
        let json = serde_json::to_string(&DisappearingTimer::new(86400)).unwrap();
        assert_eq!(json, r#"{"seconds":86400}"#);
    }
}
