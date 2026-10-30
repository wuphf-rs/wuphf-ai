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
}

impl ChatProfile {
    /// A profile that knows nothing yet about the chat.
    pub fn new(connector_id: ConnectorId, chat_id: impl Into<String>) -> Self {
        Self {
            connector_id,
            chat_id: chat_id.into(),
            disappearing: None,
        }
    }
}

/// A connector's report about one chat. `None` fields were not reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUpdate {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<DisappearingTimer>,
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
    fn a_timer_without_a_setting_time_stores_no_timestamp_key() {
        let json = serde_json::to_string(&DisappearingTimer::new(86400)).unwrap();
        assert_eq!(json, r#"{"seconds":86400}"#);
    }
}
