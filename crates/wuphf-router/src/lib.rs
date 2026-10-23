use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    AddressSet, ChatRef, Connector, ConnectorId, EndpointId, MessageId, Rule, RuleEnd, RuleId,
    RuleKind,
};
use wuphf_storage::{Storage, StorageError};

mod routing;

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

pub struct Router<S> {
    storage: Arc<S>,
    config: Arc<RoutingConfig>,
}

impl<S> Router<S>
where
    S: Storage,
{
    pub fn new(storage: Arc<S>, config: Arc<RoutingConfig>) -> Self {
        Self { storage, config }
    }
}

fn is_mirror_side(rule: &Rule, connector: &ConnectorId, container: &str) -> bool {
    match &rule.flow {
        RuleKind::OneToOne { .. } => false,
        RuleKind::ManyToOne {
            destination_connector,
            destination_chat,
            ..
        } => destination_connector == connector && destination_chat.chat_id == container,
    }
}

#[derive(Debug, Error)]
pub enum RouterError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("origin endpoint {0} is not a member of the replica set")]
    OriginOutsideReplicaSet(EndpointId),
    #[error("reply parent message {0} is not stored")]
    MessageNotFound(MessageId),
}
