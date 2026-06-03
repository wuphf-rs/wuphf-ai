use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    ConnectorCapabilities, ConnectorId, DeliveryOptions, DispatchCommand, EndpointId, Message,
    MessageId, Platform, PlatformAddress, ReplyContext, Rule, RuleId, SideKind,
};
use wuphf_storage::{Storage, StorageError};

mod routing;

/// Static per-connector facts the core learns at registration: the platform it
/// speaks and the capabilities it advertises (used to gate mirror sides).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorInfo {
    pub platform: Platform,
    pub capabilities: ConnectorCapabilities,
    /// Server-owned policy for how this connector renders replicated
    /// operations on its platform (e.g. edit/delete rendering).
    pub delivery: DeliveryOptions,
}

/// The rule set plus connector metadata that drives address resolution and
/// thread provisioning. Shared by the core and the router.
pub struct RoutingConfig {
    rules: Vec<Rule>,
    connectors: HashMap<ConnectorId, ConnectorInfo>,
}

/// The side of a rule that an inbound address matched.
pub struct MatchedSide<'a> {
    pub rule: &'a Rule,
    pub origin_side: usize,
}

impl RoutingConfig {
    pub fn new(rules: Vec<Rule>, connectors: HashMap<ConnectorId, ConnectorInfo>) -> Self {
        Self { rules, connectors }
    }

    pub fn rule(&self, id: &RuleId) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == *id)
    }

    pub fn connector(&self, id: &ConnectorId) -> Option<&ConnectorInfo> {
        self.connectors.get(id)
    }

    /// The delivery policy configured for a connector, or the default when the
    /// connector is unknown or left unconfigured.
    pub fn delivery(&self, id: &ConnectorId) -> DeliveryOptions {
        self.connector(id)
            .map(|info| info.delivery)
            .unwrap_or_default()
    }

    /// The first rule side whose connector and selector accept this address.
    pub fn match_source(
        &self,
        connector: &ConnectorId,
        address: &PlatformAddress,
    ) -> Option<MatchedSide<'_>> {
        for rule in &self.rules {
            for (index, side) in rule.sides.iter().enumerate() {
                if side.connector != *connector {
                    continue;
                }
                if side_accepts(&side.kind, address) {
                    return Some(MatchedSide {
                        rule,
                        origin_side: index,
                    });
                }
            }
        }
        None
    }
}

/// Whether a side's selector accepts a platform address.
pub fn side_accepts(kind: &SideKind, address: &PlatformAddress) -> bool {
    match kind {
        SideKind::Single { chat_id, thread_id } => {
            address.chat_id == *chat_id && address.thread_id == *thread_id
        }
        SideKind::Source { include, exclude } => {
            (include.is_empty() || include.contains(&address.chat_id))
                && !exclude.contains(&address.chat_id)
        }
        SideKind::Mirror { container } => address.chat_id == *container,
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
    rule.sides.iter().any(|side| {
        side.connector == *connector
            && matches!(&side.kind, SideKind::Mirror { container: c } if c == container)
    })
}

fn reply_context(message: Message, platform_message_id: String) -> ReplyContext {
    ReplyContext {
        message_id: message.id,
        platform_message_id,
        sender: message.sender,
        participant: message.participant,
        text: message.text,
        attachments: message.attachments,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingOutcome {
    pub dispatches: Vec<DispatchCommand>,
    pub blocked: Vec<BlockedDispatch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedDispatch {
    pub command: DispatchCommand,
    pub waiting_for_message: MessageId,
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
