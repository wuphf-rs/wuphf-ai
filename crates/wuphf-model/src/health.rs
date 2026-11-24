//! Health checks (plan 079): what connectors report about the chats the rules
//! use, the tools each process needs, and the problems the core finds in them.
//!
//! Connectors report facts only ([`ChatFacts`], [`ToolStatus`]); the core
//! decides what is wrong ([`Problem`]) and which directions of a rule pause
//! ([`HealthPause`]).

use crate::{ConnectorId, RuleId};
use serde::{Deserialize, Serialize};

/// One thing a connector may or may not know about a chat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fact {
    #[default]
    Unknown,
    Yes,
    No,
}

impl Fact {
    pub fn is_no(self) -> bool {
        self == Fact::No
    }
}

/// What this account can do in one chat, as its connector reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatFacts {
    /// The chat exists and the account can see it.
    pub exists: Fact,
    /// The account is in it.
    pub member: Fact,
    /// The account may post in it.
    pub can_send: Fact,
    /// It has topics (a Telegram forum).
    pub forum: Fact,
    /// The account may create topics in it.
    pub can_create_topics: Fact,
    /// The account may pin messages in it.
    pub can_pin: Fact,
    /// The account may delete other people's messages in it.
    pub can_delete_others: Fact,
    /// The platform's own words for a `No` or a failed lookup.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// An external program a process needs, and whether it was found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStatus {
    pub name: String,
    pub available: bool,
    /// What goes without it ("sticker pack names").
    pub needed_for: String,
}

/// A direction of a rule: `Forward` carries the source's messages into the
/// destination, `Back` carries what is typed at the destination (a reply in
/// a mirror topic) back to the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowDirection {
    Forward,
    Back,
}

impl FlowDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            FlowDirection::Forward => "forward",
            FlowDirection::Back => "back",
        }
    }
}

/// One direction of one rule held because a chat it delivers into failed a
/// check: the destination container for `Forward`, a source chat for `Back`.
/// Messages for it are held, not sent, until the check passes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HealthPause {
    pub rule: RuleId,
    pub direction: FlowDirection,
    /// The connector of the chat that failed (the one delivered into).
    pub connector: ConnectorId,
    pub chat_id: String,
}

/// What a problem does: pause a direction of a rule, or only warn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Pause(HealthPause),
    Warn,
}

/// What a problem is about, which says when it can be re-checked: a chat
/// only when its connector answers a probe about it, a process only while
/// it is connected. Until then the problem stands (an unknown neither
/// pauses nor resumes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    Chat {
        connector: ConnectorId,
        chat_id: String,
    },
    /// A connector or provider, by id (its tools).
    Process(String),
    /// The configuration and what is connected, re-checked every time.
    Wiring,
}

/// Something wrong the owner should know about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// Stable across checks, so it is reported once and can be ignored:
    /// `<check>:<what>…`, e.g. `target-not-forum:mirror:telegram-main:-100`.
    pub key: String,
    pub severity: Severity,
    pub subject: Subject,
    /// What the owner reads.
    pub text: String,
}

impl Problem {
    pub fn pause(&self) -> Option<&HealthPause> {
        match &self.severity {
            Severity::Pause(pause) => Some(pause),
            Severity::Warn => None,
        }
    }
}
