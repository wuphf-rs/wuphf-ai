//! Lazy topology resolution: turn a raw inbound address into its endpoint,
//! minting the endpoint, conversation, and replica set on first sighting when a
//! rule matches.

use crate::{Core, CoreError, identity_side_index};
use wuphf_model::{
    Alias, ConnectorId, Conversation, ConversationId, Endpoint, EndpointId, PlatformAddress,
    ReplicaSet, ReplicaSetId, Rule, SideKind, SourceAddress,
};
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    /// Resolve a raw inbound address to its endpoint, lazily minting the
    /// endpoint, conversation, and replica set on first sighting when a
    /// rule matches. Returns `None` when the address matches no rule, or matches
    /// only a mirror side with no provisioned thread (an unmapped topic).
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
        if matches!(
            rule.sides[matched.origin_side].kind,
            SideKind::Mirror { .. }
        ) {
            return Ok(None);
        }

        let identity_side = identity_side_index(&rule);
        let identity_address = match &rule.sides[identity_side].kind {
            SideKind::Single { chat_id, thread_id } => {
                PlatformAddress::new(chat_id.clone(), thread_id.clone())
            }
            SideKind::Source { .. } => source.address.clone(),
            SideKind::Mirror { .. } => unreachable!("identity side is never a mirror"),
        };

        let conversation = self
            .ensure_conversation(&rule, identity_side, &identity_address)
            .await?;

        let mut origin = None;
        for index in 0..rule.sides.len() {
            let endpoint = self
                .ensure_side_endpoint(&conversation, &rule, index, &identity_address)
                .await?;
            if index == matched.origin_side {
                origin = Some(endpoint);
            }
        }
        Ok(origin)
    }

    /// Get the conversation backing this rule's identity address, creating it
    /// (with an empty replica set) on first sighting.
    async fn ensure_conversation(
        &self,
        rule: &Rule,
        identity_side: usize,
        identity_address: &PlatformAddress,
    ) -> Result<Conversation, CoreError> {
        let identity_connector = rule.sides[identity_side].connector.clone();
        if let Some(existing) = self
            .storage
            .conversation_for_source(&rule.id, &identity_connector, identity_address)
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
            rule_id: rule.id.clone(),
            replica_set_id,
            source_connector: identity_connector,
            source_address: identity_address.clone(),
            title: None,
        };
        self.storage.put_conversation(conversation.clone()).await?;
        Ok(conversation)
    }

    /// Get or mint the endpoint for one rule side, joining it to the
    /// conversation's replica set. Single and source sides record a canonical alias;
    /// a mirror side gets no alias until its thread is provisioned.
    async fn ensure_side_endpoint(
        &self,
        conversation: &Conversation,
        rule: &Rule,
        side_index: usize,
        identity_address: &PlatformAddress,
    ) -> Result<EndpointId, CoreError> {
        let side = &rule.sides[side_index];
        let (address, with_alias) = match &side.kind {
            SideKind::Single { chat_id, thread_id } => (
                PlatformAddress::new(chat_id.clone(), thread_id.clone()),
                true,
            ),
            SideKind::Source { .. } => (identity_address.clone(), true),
            SideKind::Mirror { container } => {
                (PlatformAddress::new(container.clone(), None), false)
            }
        };

        if with_alias {
            if let Some(endpoint) = self
                .storage
                .resolve_endpoint(&side.connector, &address)
                .await?
            {
                return Ok(endpoint);
            }
        } else if let Some(endpoint) = self
            .endpoint_in_replica_set(conversation.replica_set_id, &side.connector)
            .await?
        {
            return Ok(endpoint);
        }

        let info = self
            .config
            .connector(&side.connector)
            .ok_or_else(|| CoreError::UnknownConnector(side.connector.clone()))?;
        let endpoint = Endpoint {
            id: EndpointId::new(),
            connector_id: side.connector.clone(),
            platform: info.platform.clone(),
            chat_id: address.chat_id.clone(),
            thread_id: address.thread_id.clone(),
        };
        self.storage.put_endpoint(endpoint.clone()).await?;
        if with_alias {
            self.storage
                .put_alias(Alias {
                    connector_id: side.connector.clone(),
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
