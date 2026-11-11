//! Routing topology: how rules select chats and how those resolve to the
//! endpoints and conversations a message fans out across.

use crate::*;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A messaging platform a connector bridges. `Other` keeps the model open to
/// platforms without a dedicated variant.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    WhatsApp,
    Telegram,
    Discord,
    Signal,
    Slack,
    Matrix,
    Instagram,
    Other(String),
}

/// The lowercase name a config file uses for the platform: `whatsapp`,
/// `telegram`, …, or `other:{name}`. [`FromStr`] parses it back.
impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Platform::WhatsApp => "whatsapp",
            Platform::Telegram => "telegram",
            Platform::Discord => "discord",
            Platform::Signal => "signal",
            Platform::Slack => "slack",
            Platform::Matrix => "matrix",
            Platform::Instagram => "instagram",
            Platform::Other(name) => return write!(f, "other:{name}"),
        };
        f.write_str(name)
    }
}

impl FromStr for Platform {
    type Err = UnknownPlatform;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "whatsapp" => Ok(Platform::WhatsApp),
            "telegram" => Ok(Platform::Telegram),
            "discord" => Ok(Platform::Discord),
            "signal" => Ok(Platform::Signal),
            "slack" => Ok(Platform::Slack),
            "matrix" => Ok(Platform::Matrix),
            "instagram" => Ok(Platform::Instagram),
            _ => match name.strip_prefix("other:") {
                Some(other) => Ok(Platform::Other(other.to_owned())),
                None => Err(UnknownPlatform(name.to_owned())),
            },
        }
    }
}

/// A platform name that is neither a known platform nor `other:{name}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported platform {0}")]
pub struct UnknownPlatform(pub String);

/// A concrete delivery destination on one connector: a chat, optionally a
/// thread within it. Endpoints are members of a [`Conversation`] and are
/// derived from rules at runtime, never hand-configured. `chat_id`/`thread_id`
/// hold the canonical platform address; other addresses for the same endpoint
/// are reachable through [`Alias`]es.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: EndpointId,
    pub connector_id: ConnectorId,
    pub platform: Platform,
    pub chat_id: String,
    pub thread_id: Option<String>,
}

impl Endpoint {
    pub fn address(&self) -> ChatAddress {
        ChatAddress {
            chat_id: self.chat_id.clone(),
            thread_id: self.thread_id.clone(),
        }
    }
}

/// A platform-native chat coordinate. `chat_id` identifies the chat/peer/group;
/// `thread_id` selects a thread/topic within it when the platform supports one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatAddress {
    pub chat_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

impl ChatAddress {
    pub fn new(chat_id: impl Into<String>, thread_id: Option<String>) -> Self {
        Self {
            chat_id: chat_id.into(),
            thread_id,
        }
    }

    /// Whether this address, used as a rule's chat selector, names the chat
    /// known by any of `addresses`. Chat ids must match; a selector with no
    /// thread names the whole chat, one with a thread names only that thread.
    /// Any form in the set counts, so a rule written with one of a chat's
    /// addresses still names it when the platform reports another.
    pub fn selects(&self, addresses: &AddressSet) -> bool {
        addresses.iter().any(|address| {
            self.chat_id == address.chat_id
                && (self.thread_id.is_none() || self.thread_id == address.thread_id)
        })
    }
}

/// A chat on a specific connector. Flattened when (de)serialized, so stored
/// JSON spells it as `connector_id`, `chat_id` and `thread_id` side by side;
/// the config file writes its text form (see the `Display` impl below).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatRef {
    pub connector_id: ConnectorId,
    #[serde(flatten)]
    pub address: ChatAddress,
}

/// The text form of a chat reference, `connector:chat` or
/// `connector:chat#thread`, as the config file and the settings panel write
/// it. The connector ends at the first `:` (connector ids may not contain one;
/// chat ids often do). Inside the chat and thread parts `%` is written `%25`
/// and `#` is written `%23`, so the thread separator is unambiguous. Stored
/// JSON does not use this form: `ChatRef` still serializes as fields.
impl fmt::Display for ChatRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}",
            self.connector_id.0,
            escape_chat_part(&self.address.chat_id)
        )?;
        if let Some(thread) = &self.address.thread_id {
            write!(f, "#{}", escape_chat_part(thread))?;
        }
        Ok(())
    }
}

impl FromStr for ChatRef {
    type Err = ChatRefParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((connector, rest)) = text.split_once(':').filter(|(c, _)| !c.is_empty()) else {
            return Err(ChatRefParseError::MissingConnector(text.to_owned()));
        };
        let (chat, thread) = match rest.rsplit_once('#') {
            Some((chat, thread)) => (chat, Some(thread)),
            None => (rest, None),
        };
        if chat.is_empty() {
            return Err(ChatRefParseError::EmptyChat(text.to_owned()));
        }
        if thread.is_some_and(str::is_empty) {
            return Err(ChatRefParseError::EmptyThread(text.to_owned()));
        }
        let unescape = |part: &str| {
            unescape_chat_part(part)
                .ok_or_else(|| ChatRefParseError::InvalidEscape(text.to_owned()))
        };
        Ok(ChatRef {
            connector_id: ConnectorId(connector.to_owned()),
            address: ChatAddress {
                chat_id: unescape(chat)?,
                thread_id: thread.map(unescape).transpose()?,
            },
        })
    }
}

/// Why a chat reference string did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatRefParseError {
    #[error("{0:?} has no connector: write it as connector:chat")]
    MissingConnector(String),
    #[error("{0:?} has an empty chat id")]
    EmptyChat(String),
    #[error("{0:?} has an empty thread id after '#'")]
    EmptyThread(String),
    #[error("{0:?} has an invalid escape; only %23 (#) and %25 (%) are allowed")]
    InvalidEscape(String),
}

/// Whether `id` can name a connector in a chat reference: non-empty, and
/// without `:`, `#`, `%` or whitespace, the characters the text form uses.
pub fn is_valid_connector_id(id: &str) -> bool {
    !id.is_empty()
        && !id
            .chars()
            .any(|c| matches!(c, ':' | '#' | '%') || c.is_whitespace())
}

fn escape_chat_part(part: &str) -> String {
    part.replace('%', "%25").replace('#', "%23")
}

/// Decodes exactly `%23` and `%25`; any other `%` sequence is an error, so
/// every value has one spelling.
fn unescape_chat_part(part: &str) -> Option<String> {
    let mut decoded = String::with_capacity(part.len());
    let mut rest = part;
    while let Some(index) = rest.find('%') {
        decoded.push_str(&rest[..index]);
        let escape = rest.get(index..index + 3)?;
        decoded.push(match escape {
            "%23" => '#',
            "%25" => '%',
            _ => return None,
        });
        rest = &rest[index + 3..];
    }
    decoded.push_str(rest);
    Some(decoded)
}

/// Every platform address known to name one chat: the address an event actually
/// arrived on (the primary) plus the equivalent forms the platform has reported
/// for it, as recorded by [`AddressLink`].
///
/// Platforms hand out several interchangeable addresses for the same peer —
/// WhatsApp reaches a contact as both a phone JID and a LID — and a user writing
/// a rule picks whichever form they know. Matching a rule against the whole set
/// rather than one string is what makes a chat list agree with the platform no
/// matter which form the user wrote or the connector reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressSet {
    primary: ChatAddress,
    alternates: Vec<ChatAddress>,
}

impl AddressSet {
    /// A set with no known alternates. Every connector that hands out one stable
    /// address per chat resolves through this.
    pub fn single(primary: ChatAddress) -> Self {
        Self {
            primary,
            alternates: Vec::new(),
        }
    }

    /// A set of known-equivalent forms. `alternates` may repeat the primary or
    /// each other; duplicates are dropped so callers can pass a raw closure.
    pub fn new(primary: ChatAddress, alternates: Vec<ChatAddress>) -> Self {
        let mut set = Self::single(primary);
        for address in alternates {
            set.insert(address);
        }
        set
    }

    /// Add an equivalent form, ignoring one already in the set.
    pub fn insert(&mut self, address: ChatAddress) {
        if address != self.primary && !self.alternates.contains(&address) {
            self.alternates.push(address);
        }
    }

    /// The address the event arrived on. This is the form the core stores and
    /// routes with; the alternates only widen matching.
    pub fn primary(&self) -> &ChatAddress {
        &self.primary
    }

    /// Every form in the set, primary first.
    pub fn iter(&self) -> impl Iterator<Item = &ChatAddress> {
        std::iter::once(&self.primary).chain(self.alternates.iter())
    }
}

impl From<ChatAddress> for AddressSet {
    fn from(address: ChatAddress) -> Self {
        Self::single(address)
    }
}

/// A platform-reported equivalence between two addresses on one connector (e.g.
/// a WhatsApp phone JID and the LID for the same contact). Recorded whether or
/// not either address has an [`Endpoint`] yet, which is what lets rule matching
/// treat the forms as one chat from the very first message — an [`Alias`], by
/// contrast, only exists once an endpoint does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressLink {
    pub connector_id: ConnectorId,
    pub address: ChatAddress,
    pub linked_address: ChatAddress,
}

/// A user-declared replication mapping. Rules are the only routing input a user
/// writes; conversations and endpoints are derived from them lazily at
/// runtime. The flow names which side owns conversation identity (the
/// source); replication itself stays two-way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub flow: RuleKind,
}

/// The shape of a rule: which chats on which connectors mirror each other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum RuleKind {
    /// One fixed source chat mirrored into one fixed destination chat.
    OneToOne {
        source_connector: ConnectorId,
        source_chat: ChatAddress,

        destination_connector: ConnectorId,
        destination_chat: ChatAddress,
    },
    /// Every chat on the source connector that passes the filter, each mirrored
    /// into its own thread auto-created inside the destination container chat.
    /// The destination's `thread_id` is ignored: threads are provisioned per
    /// source conversation.
    ManyToOne {
        source_connector: ConnectorId,
        #[serde(default)]
        source_chats_filter: ChatFilter,

        destination_connector: ConnectorId,
        destination_chat: ChatAddress,
    },
}

/// Which source chats a many-to-one rule covers. Struct variants (not tuples)
/// because serde cannot internally tag a newtype variant holding a sequence,
/// and the config lives in TOML.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum ChatFilter {
    #[default]
    All,
    Only {
        chats: Vec<ChatAddress>,
    },
    Not {
        chats: Vec<ChatAddress>,
    },
}

impl ChatFilter {
    /// Whether a chat, named by every address form known for it, passes. Both
    /// list kinds test the whole set: an allow list admits a chat listed under
    /// any of its forms, and a deny list excludes one listed under any of its
    /// forms — so a chat cannot slip past a deny list by arriving under the
    /// address the user did not write.
    pub fn accepts(&self, addresses: &AddressSet) -> bool {
        match self {
            ChatFilter::All => true,
            ChatFilter::Only { chats } => chats.iter().any(|chat| chat.selects(addresses)),
            ChatFilter::Not { chats } => !chats.iter().any(|chat| chat.selects(addresses)),
        }
    }
}

/// One of the two ends of a rule's flow. The source end owns conversation
/// identity; replication itself is two-way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleEnd {
    Source,
    Destination,
}

impl RuleKind {
    /// The connector serving the given end of this flow.
    pub fn connector(&self, end: RuleEnd) -> &ConnectorId {
        match (self, end) {
            (
                RuleKind::OneToOne {
                    source_connector, ..
                },
                RuleEnd::Source,
            )
            | (
                RuleKind::ManyToOne {
                    source_connector, ..
                },
                RuleEnd::Source,
            ) => source_connector,
            (
                RuleKind::OneToOne {
                    destination_connector,
                    ..
                },
                RuleEnd::Destination,
            )
            | (
                RuleKind::ManyToOne {
                    destination_connector,
                    ..
                },
                RuleEnd::Destination,
            ) => destination_connector,
        }
    }

    /// Whether the given end of this flow, served by `connector`, accepts an
    /// inbound chat, named by every address form known for it. A many-to-one
    /// destination matches on the container chat alone: its threads are minted
    /// per conversation, so a thread-level address still belongs to the
    /// container.
    pub fn end_accepts(
        &self,
        end: RuleEnd,
        connector: &ConnectorId,
        addresses: &AddressSet,
    ) -> bool {
        if self.connector(end) != connector {
            return false;
        }
        match (self, end) {
            (RuleKind::OneToOne { source_chat, .. }, RuleEnd::Source) => {
                source_chat.selects(addresses)
            }
            (
                RuleKind::OneToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => destination_chat.selects(addresses),
            (
                RuleKind::ManyToOne {
                    source_chats_filter,
                    ..
                },
                RuleEnd::Source,
            ) => source_chats_filter.accepts(addresses),
            (
                RuleKind::ManyToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => addresses
                .iter()
                .any(|address| destination_chat.chat_id == address.chat_id),
        }
    }
}

/// A resolvable platform address for an endpoint. Many aliases may point at one
/// endpoint (e.g. after a WhatsApp phone-to-LID migration); exactly one is
/// canonical for outbound sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    pub connector_id: ConnectorId,
    pub address: ChatAddress,
    pub endpoint_id: EndpointId,
    pub is_canonical: bool,
}

/// A set of chats that mirror each other: the unit a rule creates for one
/// source chat. A [`Message`] arriving at any endpoint is dispatched to all the
/// others, never back to the origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    /// The rule that created it. `None` for a chat outside every rule that the
    /// owner sent a message to directly.
    pub rule_id: Option<RuleId>,
    /// The chat the conversation is named after: the rule's source chat, or
    /// the chat the owner sent to.
    pub source: ChatRef,
    pub endpoints: Vec<EndpointId>,
    /// The last source title persisted for this conversation. Tracked so the
    /// core can detect when a source chat is renamed and fan a rename out to
    /// the mirror threads. `None` until a title is first observed.
    #[serde(default)]
    pub title: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_ref(connector: &str, chat: &str, thread: Option<&str>) -> ChatRef {
        ChatRef {
            connector_id: ConnectorId(connector.into()),
            address: ChatAddress::new(chat, thread.map(Into::into)),
        }
    }

    fn named_platforms() -> [(&'static str, Platform); 7] {
        [
            ("whatsapp", Platform::WhatsApp),
            ("telegram", Platform::Telegram),
            ("discord", Platform::Discord),
            ("signal", Platform::Signal),
            ("slack", Platform::Slack),
            ("matrix", Platform::Matrix),
            ("instagram", Platform::Instagram),
        ]
    }

    #[test]
    fn every_named_platform_parses() {
        for (name, platform) in named_platforms() {
            assert_eq!(name.parse::<Platform>(), Ok(platform), "{name}");
        }
    }

    #[test]
    fn other_prefix_parses_to_other() {
        assert_eq!(
            "other:mattermost".parse::<Platform>(),
            Ok(Platform::Other("mattermost".into()))
        );
    }

    #[test]
    fn unknown_platform_is_rejected() {
        let error = "unknown".parse::<Platform>().unwrap_err();
        assert_eq!(error, UnknownPlatform("unknown".into()));
        assert_eq!(error.to_string(), "unsupported platform unknown");
    }

    #[test]
    fn platforms_round_trip_through_display() {
        let platforms = named_platforms()
            .into_iter()
            .map(|(_, platform)| platform)
            .chain([Platform::Other("mattermost".into())]);
        for platform in platforms {
            assert_eq!(platform.to_string().parse::<Platform>(), Ok(platform));
        }
    }

    #[test]
    fn chat_refs_round_trip() {
        let cases = [
            (
                "telegram-main:telegram:channel:2060340733",
                chat_ref("telegram-main", "telegram:channel:2060340733", None),
            ),
            (
                "telegram-main:telegram:channel:2060340733#12",
                chat_ref("telegram-main", "telegram:channel:2060340733", Some("12")),
            ),
            (
                "whatsapp-main:12345@s.whatsapp.net",
                chat_ref("whatsapp-main", "12345@s.whatsapp.net", None),
            ),
            (
                "whatsapp-main:98765@lid",
                chat_ref("whatsapp-main", "98765@lid", None),
            ),
            (
                "whatsapp-main:120363@g.us",
                chat_ref("whatsapp-main", "120363@g.us", None),
            ),
            (
                "instagram-main:340282366841710300949128",
                chat_ref("instagram-main", "340282366841710300949128", None),
            ),
            (
                "matrix-main:!room:example.org",
                chat_ref("matrix-main", "!room:example.org", None),
            ),
            (
                "matrix-main:%23alias:example.org",
                chat_ref("matrix-main", "#alias:example.org", None),
            ),
            (
                "matrix-main:50%25off#t%23%25",
                chat_ref("matrix-main", "50%off", Some("t#%")),
            ),
        ];
        for (text, expected) in cases {
            let parsed: ChatRef = text
                .parse()
                .unwrap_or_else(|error| panic!("{text}: {error}"));
            assert_eq!(parsed, expected, "{text}");
            assert_eq!(parsed.to_string(), text);
        }
    }

    #[test]
    fn chat_ref_parse_errors() {
        let cases = [
            (
                "no-colon",
                ChatRefParseError::MissingConnector("no-colon".into()),
            ),
            (":chat", ChatRefParseError::MissingConnector(":chat".into())),
            ("conn:", ChatRefParseError::EmptyChat("conn:".into())),
            ("conn:#7", ChatRefParseError::EmptyChat("conn:#7".into())),
            (
                "conn:chat#",
                ChatRefParseError::EmptyThread("conn:chat#".into()),
            ),
            (
                "conn:a%2Fb",
                ChatRefParseError::InvalidEscape("conn:a%2Fb".into()),
            ),
            (
                "conn:a%2",
                ChatRefParseError::InvalidEscape("conn:a%2".into()),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(text.parse::<ChatRef>(), Err(expected), "{text}");
        }
    }

    #[test]
    fn chat_ref_json_shape_is_unchanged() {
        let value = serde_json::to_value(chat_ref("tg", "-100", Some("12"))).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "connector_id": "tg", "chat_id": "-100", "thread_id": "12" })
        );
    }

    #[test]
    fn connector_ids_exclude_the_separators() {
        for valid in [
            "whatsapp-main",
            "whatsapp-rust",
            "instagram-main",
            "telegram-main",
        ] {
            assert!(is_valid_connector_id(valid), "{valid}");
        }
        for invalid in ["", "tg:main", "tg#main", "tg%main", "tg main", "tg\tmain"] {
            assert!(!is_valid_connector_id(invalid), "{invalid:?}");
        }
    }
}
