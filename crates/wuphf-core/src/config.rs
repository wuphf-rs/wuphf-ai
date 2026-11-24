//! The rule set, connector list and owner-notice targets the core routes by.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    AddressSet, ChatRef, ChatStateDirection, ChatStatePolicy, ChatStatePolicyOverride, Connector,
    ConnectorId, Conversation, Rule, RuleEnd, RuleId,
};

/// The rule set plus connector metadata that drives address resolution and
/// thread provisioning. Shared by the core and the router.
pub struct RoutingConfig {
    rules: Vec<Rule>,
    connectors: HashMap<ConnectorId, Connector>,
    /// Where owner notices are delivered: chats on connectors expected to still
    /// be working. Several may be configured, since the point is to reach the
    /// owner somewhere other than the connector in trouble.
    notice_targets: Vec<ChatRef>,
    /// Which way pin, mute and archive cross, globally and per conversation.
    chat_state: ChatStateSettings,
}

/// The `chat_state` settings: a global policy, and overrides that each name a
/// chat (a conversation's source chat or its mirror topic).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatStateSettings {
    pub global: ChatStatePolicy,
    pub overrides: Vec<(ChatRef, ChatStatePolicyOverride)>,
}

/// Why a routing config is invalid. Serde checks the shape of each rule; these
/// are the checks that span rules and the connector list.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoutingConfigError {
    #[error("rule {rule} names connector {connector}, which is not configured")]
    UnknownRuleConnector {
        rule: RuleId,
        connector: ConnectorId,
    },
    #[error("rule id {0} is used more than once")]
    DuplicateRule(RuleId),
    #[error("notice target names connector {0}, which is not configured")]
    UnknownNoticeConnector(ConnectorId),
    #[error("a chat_state override names connector {0}, which is not configured")]
    UnknownChatStateConnector(ConnectorId),
}

/// The end of a rule that an inbound address matched.
pub struct MatchedRule<'a> {
    pub rule: &'a Rule,
    pub origin: RuleEnd,
}

impl RoutingConfig {
    pub fn new(rules: Vec<Rule>, connectors: Vec<Connector>) -> Self {
        Self {
            rules,
            connectors: connectors
                .into_iter()
                .map(|connector| (connector.id.clone(), connector))
                .collect(),
            notice_targets: Vec::new(),
            chat_state: ChatStateSettings::default(),
        }
    }

    /// Declare which way chat states cross. Without it, every state crosses
    /// both ways.
    pub fn with_chat_state(mut self, chat_state: ChatStateSettings) -> Self {
        self.chat_state = chat_state;
        self
    }

    /// The chat-state policy of one conversation: the first override whose
    /// chat is the conversation's source or one of its endpoints, field by
    /// field over the global policy.
    pub fn chat_state_policy(
        &self,
        conversation: &Conversation,
        endpoints: &[ChatRef],
    ) -> ChatStatePolicy {
        let names = |chat: &ChatRef| *chat == conversation.source || endpoints.contains(chat);
        match self
            .chat_state
            .overrides
            .iter()
            .find(|(chat, _)| names(chat))
        {
            Some((_, override_)) => self.chat_state.global.with(override_),
            None => self.chat_state.global,
        }
    }

    /// Declare where owner notices are delivered. Separate from [`Self::new`] so
    /// the common case — and every test that only cares about routing — stays a
    /// two-argument call.
    pub fn with_notice_targets(mut self, targets: Vec<ChatRef>) -> Self {
        self.notice_targets = targets;
        self
    }

    /// The chats an owner notice is delivered to.
    pub fn notice_targets(&self) -> impl Iterator<Item = &ChatRef> {
        self.notice_targets.iter()
    }

    /// The chats an owner notice is delivered to, excluding any served by
    /// `raising` — the connector reporting trouble is the one least likely to
    /// be able to deliver the news about it.
    pub fn notice_targets_excluding(
        &self,
        raising: &ConnectorId,
    ) -> impl Iterator<Item = &ChatRef> {
        self.notice_targets
            .iter()
            .filter(move |target| target.connector_id != *raising)
    }

    /// Checks what serde cannot: every rule and notice target names a
    /// configured connector, and rule ids are unique. Conversations remember
    /// their rule by id, so a duplicate id would make two rules share them.
    pub fn validate(&self) -> Result<(), RoutingConfigError> {
        let mut seen = HashSet::new();
        for rule in &self.rules {
            if !seen.insert(&rule.id) {
                return Err(RoutingConfigError::DuplicateRule(rule.id.clone()));
            }
            for end in [RuleEnd::Source, RuleEnd::Destination] {
                let connector = rule.flow.connector(end);
                if !self.connectors.contains_key(connector) {
                    return Err(RoutingConfigError::UnknownRuleConnector {
                        rule: rule.id.clone(),
                        connector: connector.clone(),
                    });
                }
            }
        }
        if let Some(target) = self
            .notice_targets
            .iter()
            .find(|target| !self.connectors.contains_key(&target.connector_id))
        {
            return Err(RoutingConfigError::UnknownNoticeConnector(
                target.connector_id.clone(),
            ));
        }
        if let Some((chat, _)) = self
            .chat_state
            .overrides
            .iter()
            .find(|(chat, _)| !self.connectors.contains_key(&chat.connector_id))
        {
            return Err(RoutingConfigError::UnknownChatStateConnector(
                chat.connector_id.clone(),
            ));
        }
        Ok(())
    }

    /// Every rule, in config order.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Whether pins cross between chats anywhere: globally or in an override.
    pub fn pins_synced(&self) -> bool {
        self.chat_state.global.pin != ChatStateDirection::Off
            || self
                .chat_state
                .overrides
                .iter()
                .any(|(_, policy)| policy.pin.is_some_and(|pin| pin != ChatStateDirection::Off))
    }

    pub fn rule(&self, id: &RuleId) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == *id)
    }

    pub fn connector(&self, id: &ConnectorId) -> Option<&Connector> {
        self.connectors.get(id)
    }

    /// The first rule end whose connector and selector accept this chat. The
    /// chat is named by every address form known for it, so a rule written with
    /// one form selects it however the platform addressed the event.
    pub fn match_source(
        &self,
        connector: &ConnectorId,
        addresses: &AddressSet,
    ) -> Option<MatchedRule<'_>> {
        for rule in &self.rules {
            for end in [RuleEnd::Source, RuleEnd::Destination] {
                if rule.flow.end_accepts(end, connector, addresses) {
                    return Some(MatchedRule { rule, origin: end });
                }
            }
        }
        None
    }
}

/// Where the core gets its routing config. Read once at the start of each
/// operation, so one event is routed against one consistent config even if
/// the config changes meanwhile.
pub trait RoutingSource: Send + Sync + 'static {
    fn routing(&self) -> Arc<RoutingConfig>;
}

/// A fixed config, e.g. in tests.
impl RoutingSource for Arc<RoutingConfig> {
    fn routing(&self) -> Arc<RoutingConfig> {
        Arc::clone(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{ChatAddress, ChatStateDirection, ConversationId};

    fn chat(connector: &str, chat_id: &str, thread: Option<&str>) -> ChatRef {
        ChatRef {
            connector_id: ConnectorId(connector.into()),
            address: ChatAddress::new(chat_id, thread.map(Into::into)),
        }
    }

    fn conversation(source: ChatRef) -> Conversation {
        Conversation {
            id: ConversationId::new(),
            rule_id: None,
            source,
            endpoints: Vec::new(),
            title: None,
            introduced: false,
        }
    }

    #[test]
    fn an_override_matches_the_source_or_the_mirror_topic() {
        let family = chat("whatsapp", "1@g.us", None);
        let family_topic = chat("telegram", "-100", Some("57"));
        let work_topic = chat("telegram", "-100", Some("58"));
        let config =
            RoutingConfig::new(Vec::new(), Vec::new()).with_chat_state(ChatStateSettings {
                global: ChatStatePolicy {
                    pin: ChatStateDirection::ToMirror,
                    ..ChatStatePolicy::default()
                },
                overrides: vec![
                    (
                        family.clone(),
                        ChatStatePolicyOverride {
                            mute: Some(ChatStateDirection::Off),
                            ..ChatStatePolicyOverride::default()
                        },
                    ),
                    (
                        work_topic.clone(),
                        ChatStatePolicyOverride {
                            archive: Some(ChatStateDirection::ToSource),
                            ..ChatStatePolicyOverride::default()
                        },
                    ),
                ],
            });

        // Named by its source: the override's field wins, the rest is global.
        let policy = config.chat_state_policy(&conversation(family), &[family_topic]);
        assert_eq!(policy.mute, ChatStateDirection::Off);
        assert_eq!(policy.pin, ChatStateDirection::ToMirror);
        assert_eq!(policy.archive, ChatStateDirection::Both);

        // Named by its mirror topic.
        let work = chat("whatsapp", "2@g.us", None);
        let policy = config.chat_state_policy(&conversation(work), &[work_topic]);
        assert_eq!(policy.archive, ChatStateDirection::ToSource);
        assert_eq!(policy.mute, ChatStateDirection::Both);

        // Not named: the global policy.
        let other = chat("whatsapp", "3@g.us", None);
        let topic = chat("telegram", "-100", Some("59"));
        assert_eq!(
            config.chat_state_policy(&conversation(other), &[topic]),
            ChatStatePolicy {
                pin: ChatStateDirection::ToMirror,
                ..ChatStatePolicy::default()
            }
        );
    }
}
