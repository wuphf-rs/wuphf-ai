//! Lazy topology resolution: turn a raw inbound address into its endpoint,
//! minting the endpoint, conversation, and replica set on first sighting when a
//! rule matches.

use crate::{Core, CoreError};
use wuphf_model::{
    Alias, ConnectorId, Conversation, ConversationId, Endpoint, EndpointId, PlatformAddress,
    ReplicaSet, ReplicaSetId, RoutingRuleFlow, RuleEnd, RuleId, SourceAddress,
};
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    /// Resolve a raw inbound address to its endpoint, lazily minting the
    /// endpoint, conversation, and replica set on first sighting when a rule
    /// matches. Returns `None` when the address matches no rule, or matches
    /// only a many-to-one destination with no provisioned thread (an unmapped
    /// topic in the container).
    pub(crate) async fn resolve_origin(
        &self,
        source: &SourceAddress,
    ) -> Result<Option<EndpointId>, CoreError> {
        if let Some(endpoint) = self
            .storage
            .resolve_endpoint(&source.connector_id, &source.address)
            .await?
        {
            return Ok(Some(endpoint));
        }

        let Some(matched) = self
            .config
            .match_source(&source.connector_id, &source.address)
        else {
            return Ok(None);
        };
        let rule = matched.rule.clone();
        let origin_end = matched.origin;
        // A first sighting arriving on a many-to-one destination is a message
        // in a thread no conversation has claimed yet: conversations are minted
        // from the source end, so there is nothing to bind it to.
        if origin_end == RuleEnd::Destination
            && matches!(rule.flow, RoutingRuleFlow::ManyToOne { .. })
        {
            return Ok(None);
        }

        // The source end owns conversation identity: a one-to-one rule is keyed
        // by its fixed source chat, a many-to-one rule by the actual source
        // chat this event arrived on.
        let identity_address = match &rule.flow {
            RoutingRuleFlow::OneToOne { source_chat, .. } => source_chat.address(),
            RoutingRuleFlow::ManyToOne { .. } => source.address.clone(),
        };
        let identity_connector = rule.flow.connector(RuleEnd::Source).clone();

        let conversation = self
            .ensure_conversation(&rule.id, identity_connector, &identity_address)
            .await?;

        let mut origin = None;
        for end in [RuleEnd::Source, RuleEnd::Destination] {
            let endpoint = self
                .ensure_end_endpoint(&conversation, &rule.flow, end, &identity_address)
                .await?;
            if end == origin_end {
                origin = Some(endpoint);
            }
        }
        Ok(origin)
    }

    /// Get the conversation backing this rule's identity address, creating it
    /// (with an empty replica set) on first sighting.
    async fn ensure_conversation(
        &self,
        rule_id: &RuleId,
        identity_connector: ConnectorId,
        identity_address: &PlatformAddress,
    ) -> Result<Conversation, CoreError> {
        if let Some(existing) = self
            .storage
            .conversation_for_source(rule_id, &identity_connector, identity_address)
            .await?
        {
            return Ok(existing);
        }
        let replica_set_id = ReplicaSetId::new();
        self.storage
            .put_replica_set(ReplicaSet {
                id: replica_set_id,
                endpoints: Vec::new(),
            })
            .await?;
        let conversation = Conversation {
            id: ConversationId::new(),
            rule_id: rule_id.clone(),
            replica_set_id,
            source_connector: identity_connector,
            source_address: identity_address.clone(),
            title: None,
        };
        self.storage.put_conversation(conversation.clone()).await?;
        Ok(conversation)
    }

    /// Get or mint the endpoint for one end of a rule's flow, joining it to the
    /// conversation's replica set. Fixed chats and many-to-one sources record a
    /// canonical alias; a many-to-one destination gets no alias until its
    /// thread is provisioned, so it is found through the replica set instead.
    async fn ensure_end_endpoint(
        &self,
        conversation: &Conversation,
        flow: &RoutingRuleFlow,
        end: RuleEnd,
        identity_address: &PlatformAddress,
    ) -> Result<EndpointId, CoreError> {
        let connector = flow.connector(end).clone();
        let (address, with_alias) = match (flow, end) {
            (RoutingRuleFlow::OneToOne { source_chat, .. }, RuleEnd::Source) => {
                (source_chat.address(), true)
            }
            (
                RoutingRuleFlow::OneToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => (destination_chat.address(), true),
            (RoutingRuleFlow::ManyToOne { .. }, RuleEnd::Source) => {
                (identity_address.clone(), true)
            }
            (
                RoutingRuleFlow::ManyToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => (
                PlatformAddress::new(destination_chat.chat_id.clone(), None),
                false,
            ),
        };

        if with_alias {
            if let Some(endpoint) = self.storage.resolve_endpoint(&connector, &address).await? {
                return Ok(endpoint);
            }
        } else if let Some(endpoint) = self
            .endpoint_in_replica_set(conversation.replica_set_id, &connector)
            .await?
        {
            return Ok(endpoint);
        }

        let info = self
            .config
            .connector(&connector)
            .ok_or_else(|| CoreError::UnknownConnector(connector.clone()))?;
        let endpoint = Endpoint {
            id: EndpointId::new(),
            connector_id: connector.clone(),
            platform: info.platform.clone(),
            chat_id: address.chat_id.clone(),
            thread_id: address.thread_id.clone(),
        };
        self.storage.put_endpoint(endpoint.clone()).await?;
        if with_alias {
            self.storage
                .put_alias(Alias {
                    connector_id: connector,
                    address,
                    endpoint_id: endpoint.id,
                    is_canonical: true,
                })
                .await?;
        }
        let mut replica_set = self
            .storage
            .replica_set(conversation.replica_set_id)
            .await?;
        if !replica_set.endpoints.contains(&endpoint.id) {
            replica_set.endpoints.push(endpoint.id);
            self.storage.put_replica_set(replica_set).await?;
        }
        Ok(endpoint.id)
    }

    async fn endpoint_in_replica_set(
        &self,
        replica_set_id: ReplicaSetId,
        connector: &ConnectorId,
    ) -> Result<Option<EndpointId>, CoreError> {
        let replica_set = self.storage.replica_set(replica_set_id).await?;
        for endpoint_id in replica_set.endpoints {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            if endpoint.connector_id == *connector {
                return Ok(Some(endpoint_id));
            }
        }
        Ok(None)
    }
}
