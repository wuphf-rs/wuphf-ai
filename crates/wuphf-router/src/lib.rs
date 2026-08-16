use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    AddressSet, ConnectorId, ConnectorInfo, EndpointId, MessageId, RoutingRule, RoutingRuleFlow,
    RuleEnd, RuleId,
};
use wuphf_storage::{Storage, StorageError};

mod routing;

/// The rule set plus connector metadata that drives address resolution and
/// thread provisioning. Shared by the core and the router.
pub struct RoutingConfig {
    rules: Vec<RoutingRule>,
    connectors: HashMap<ConnectorId, ConnectorInfo>,
}

/// The end of a rule that an inbound address matched.
pub struct MatchedRule<'a> {
    pub rule: &'a RoutingRule,
    pub origin: RuleEnd,
}

impl RoutingConfig {
    pub fn new(rules: Vec<RoutingRule>, connectors: HashMap<ConnectorId, ConnectorInfo>) -> Self {
        Self { rules, connectors }
    }

    pub fn rule(&self, id: &RuleId) -> Option<&RoutingRule> {
        self.rules.iter().find(|rule| rule.id == *id)
    }

    pub fn connector(&self, id: &ConnectorId) -> Option<&ConnectorInfo> {
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

fn is_mirror_side(rule: &RoutingRule, connector: &ConnectorId, container: &str) -> bool {
    match &rule.flow {
        RoutingRuleFlow::OneToOne { .. } => false,
        RoutingRuleFlow::ManyToOne {
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
