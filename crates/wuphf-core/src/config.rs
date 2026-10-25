//! The rule set, connector list and owner-notice targets the core routes by.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{AddressSet, ChatRef, Connector, ConnectorId, Rule, RuleEnd, RuleId};

/// The rule set plus connector metadata that drives address resolution and
/// thread provisioning. Shared by the core and the router.
pub struct RoutingConfig {
    rules: Vec<Rule>,
    connectors: HashMap<ConnectorId, Connector>,
    /// Where owner notices are delivered: chats on connectors expected to still
    /// be working. Several may be configured, since the point is to reach the
    /// owner somewhere other than the connector in trouble.
    notice_targets: Vec<ChatRef>,
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
        }
    }

    /// Declare where owner notices are delivered. Separate from [`Self::new`] so
    /// the common case — and every test that only cares about routing — stays a
    /// two-argument call.
    pub fn with_notice_targets(mut self, targets: Vec<ChatRef>) -> Self {
        self.notice_targets = targets;
        self
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
        Ok(())
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
