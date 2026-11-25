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
    /// The conversation's pin, mute and archive state, kept in the source
    /// chat's profile (a mirror topic has no profile of its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<MuteState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    /// What this account may do in the chat, as its connector reports it
    /// (plan 083). `None` until reported: the chat is then open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posting: Option<ChatPosting>,
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
            pinned: None,
            mute: None,
            archived: None,
            posting: None,
        }
    }

    /// Whether this account may post new messages here: false only when the
    /// connector reported it may not.
    pub fn accepts_messages(&self) -> bool {
        self.posting.as_ref().is_none_or(|posting| posting.messages)
    }

    /// Whether this account may react here.
    pub fn accepts_reactions(&self) -> bool {
        self.posting
            .as_ref()
            .is_none_or(|posting| posting.reactions)
    }

    /// Whether a message here may be quoted only in its author's direct
    /// chat (a story).
    pub fn private_replies(&self) -> bool {
        self.posting
            .as_ref()
            .is_some_and(|posting| posting.private_replies)
    }

    /// Whether this account may post here on purpose (plan 084): a story, a
    /// channel post. Only when the connector reported it.
    pub fn accepts_posts(&self) -> bool {
        self.posting.as_ref().is_some_and(|posting| posting.posts)
    }
}

/// What an account may do in a chat (plan 083), as its connector reports it:
/// a WhatsApp story feed takes reactions only, and a story may be quoted only
/// in its poster's chat; an admin-only group takes no messages from a
/// member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPosting {
    /// The account may post new messages (and edit its own).
    pub messages: bool,
    /// The account may react to messages.
    pub reactions: bool,
    /// A message here may be quoted only in a reply to its author's direct
    /// chat.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private_replies: bool,
    /// The account may post here on purpose (plan 084): a story to its
    /// contacts, a channel post to its followers. Only an explicit post
    /// (`/status`, `/post`) does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub posts: bool,
    /// Who sees a post, as the platform's setting says ("My contacts",
    /// "My contacts except 3"), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

impl ChatPosting {
    /// A chat open to everything but posts.
    pub fn open() -> Self {
        Self {
            messages: true,
            reactions: true,
            private_replies: false,
            posts: false,
            audience: None,
        }
    }
}

/// A chat's mute): 0 not muted, [`MuteState::FOREVER`] muted forever,
/// otherwise the end in Unix seconds. A mute whose end has passed is 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuteState {
    pub muted_until: i64,
}

impl MuteState {
    pub const FOREVER: i64 = -1;
}

/// Which way a chat state (pin, mute or archive) crosses the bridge, named
/// from the conversation's source: `to_mirror` carries the source's state to
/// its mirror thread, `to_source` the mirror's back to the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(rename_all = "snake_case")]
pub enum ChatStateDirection {
    Off,
    ToMirror,
    ToSource,
    #[default]
    Both,
}

impl ChatStateDirection {
    /// Whether a change reported by the source side (`from_source`) or by the
    /// mirror side may cross to the other side.
    pub fn allows(self, from_source: bool) -> bool {
        match self {
            ChatStateDirection::Off => false,
            ChatStateDirection::ToMirror => from_source,
            ChatStateDirection::ToSource => !from_source,
            ChatStateDirection::Both => true,
        }
    }
}

/// The direction of each chat state for one conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatStatePolicy {
    pub pin: ChatStateDirection,
    pub mute: ChatStateDirection,
    pub archive: ChatStateDirection,
}

/// A per-conversation override: each field present replaces the global one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatStatePolicyOverride {
    pub pin: Option<ChatStateDirection>,
    pub mute: Option<ChatStateDirection>,
    pub archive: Option<ChatStateDirection>,
}

impl ChatStatePolicy {
    /// This policy with `override_`'s fields put over it.
    pub fn with(self, override_: &ChatStatePolicyOverride) -> Self {
        Self {
            pin: override_.pin.unwrap_or(self.pin),
            mute: override_.mute.unwrap_or(self.mute),
            archive: override_.archive.unwrap_or(self.archive),
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
    /// Set when the report is about a thread (a Telegram forum topic) rather
    /// than the whole chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<MuteState>,
    /// WhatsApp: archived. Telegram topic: closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    /// What this account may do in the chat (plan 083).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posting: Option<ChatPosting>,
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
    fn directions_allow_their_side() {
        use ChatStateDirection::*;
        for (direction, from_source, from_mirror) in [
            (Off, false, false),
            (ToMirror, true, false),
            (ToSource, false, true),
            (Both, true, true),
        ] {
            assert_eq!(direction.allows(true), from_source, "{direction:?}");
            assert_eq!(direction.allows(false), from_mirror, "{direction:?}");
        }
        assert_eq!(ChatStateDirection::default(), Both);
        assert_eq!(serde_json::to_string(&ToMirror).unwrap(), r#""to_mirror""#);
    }

    #[test]
    fn an_override_replaces_only_its_fields() {
        let global = ChatStatePolicy::default();
        let policy = global.with(&ChatStatePolicyOverride {
            mute: Some(ChatStateDirection::Off),
            ..ChatStatePolicyOverride::default()
        });
        assert_eq!(policy.mute, ChatStateDirection::Off);
        assert_eq!(policy.pin, ChatStateDirection::Both);
        assert_eq!(policy.archive, ChatStateDirection::Both);
    }

    #[test]
    fn a_timer_without_a_setting_time_stores_no_timestamp_key() {
        let json = serde_json::to_string(&DisappearingTimer::new(86400)).unwrap();
        assert_eq!(json, r#"{"seconds":86400}"#);
    }
}
