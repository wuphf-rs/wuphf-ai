//! Lazy topology resolution: turn a raw inbound address into its endpoint,
//! minting the endpoint, conversation, and replica set on first sighting when a
//! rule matches.

use crate::{Core, CoreError};
use wuphf_model::{
    AddressSet, Alias, ConnectorId, Conversation, ConversationId, Endpoint, EndpointId,
    ChatAddress, ReplicaSet, ReplicaSetId, RoutingRuleFlow, RuleEnd, RuleId, SourceAddress,
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
            return self.still_covered(endpoint, source).await;
        }

        // Rules are matched against every address form the platform has
        // reported for this chat, not just the one the event arrived under: a
        // chat list written with a WhatsApp phone JID has to select — or
        // exclude — the same chat when it arrives addressed by LID.
        let addresses = self
            .address_set(&source.connector_id, &source.address)
            .await?;
        let Some(matched) = self.config.match_source(&source.connector_id, &addresses) else {
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
        if let Some(origin) = origin {
            self.bind_known_forms(origin, &source.connector_id, &addresses)
                .await?;
        }
        Ok(origin)
    }

    /// Re-check an already-resolved chat against the rules, returning `None`
    /// when no rule covers it any more.
    ///
    /// Coverage is not settled at first sighting. A chat can gain an address
    /// form after it was first routed — and that form may be one a deny list
    /// names, in which case the chat slipped past the list only because the
    /// core did not yet know the two addresses were the same. The user can also
    /// edit the rules between runs. Testing on every event keeps a deny list
    /// meaning what it says instead of only applying to chats the core had
    /// never seen when the list was written.
    ///
    /// Endpoints with no conversation behind them are exempt: those are the
    /// standalone chats a raw send mints outside every rule, and they have to
    /// stay resolvable so their echoes are recognized.
    async fn still_covered(
        &self,
        endpoint_id: EndpointId,
        source: &SourceAddress,
    ) -> Result<Option<EndpointId>, CoreError> {
        let Some(replica_set_id) = self.storage.replica_set_for_endpoint(endpoint_id).await? else {
            return Ok(Some(endpoint_id));
        };
        let Some(conversation) = self
            .storage
            .conversation_for_replica_set(replica_set_id)
            .await?
        else {
            return Ok(Some(endpoint_id));
        };
        // A conversation whose rule is gone from the config is no longer
        // covered by anything, so it stops replicating.
        let Some(rule) = self.config.rule(&conversation.rule_id) else {
            return Ok(None);
        };
        let addresses = self
            .address_set(&source.connector_id, &source.address)
            .await?;
        let covered = [RuleEnd::Source, RuleEnd::Destination]
            .into_iter()
            .any(|end| rule.flow.end_accepts(end, &source.connector_id, &addresses));
        Ok(covered.then_some(endpoint_id))
    }

    /// Every platform address known to name this chat: the one the event
    /// arrived on plus the transitive closure of the equivalences the platform
    /// has reported (a chain, since a chat can migrate more than once). The
    /// closure is walked breadth-first over a visited set, so a cycle — links
    /// are stored symmetrically, so every link is one — terminates.
    pub(crate) async fn address_set(
        &self,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> Result<AddressSet, CoreError> {
        let mut addresses = AddressSet::single(address.clone());
        let mut frontier = vec![address.clone()];
        while let Some(address) = frontier.pop() {
            for linked in self
                .storage
                .linked_addresses(connector_id, &address)
                .await?
            {
                if addresses.iter().any(|known| *known == linked) {
                    continue;
                }
                addresses.insert(linked.clone());
                frontier.push(linked);
            }
        }
        Ok(addresses)
    }

    /// Make every known form of a chat resolve straight to its endpoint, so
    /// later events skip rule matching whichever address they arrive under. The
    /// endpoint keeps the canonical alias it was minted with; the other forms
    /// are added as non-canonical, leaving outbound addressing untouched.
    async fn bind_known_forms(
        &self,
        endpoint_id: EndpointId,
        connector_id: &ConnectorId,
        addresses: &AddressSet,
    ) -> Result<(), CoreError> {
        for address in addresses.iter() {
            if self
                .storage
                .resolve_endpoint(connector_id, address)
                .await?
                .is_some()
            {
                continue;
            }
            self.storage
                .put_alias(Alias {
                    connector_id: connector_id.clone(),
                    address: address.clone(),
                    endpoint_id,
                    is_canonical: false,
                })
                .await?;
        }
        Ok(())
    }

    /// Get the conversation backing this rule's identity address, creating it
    /// (with an empty replica set) on first sighting.
    async fn ensure_conversation(
        &self,
        rule_id: &RuleId,
        identity_connector: ConnectorId,
        identity_address: &ChatAddress,
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
        identity_address: &ChatAddress,
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
                ChatAddress::new(destination_chat.chat_id.clone(), None),
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
