//! Lazy topology resolution: turn a raw inbound address into its endpoint,
//! minting the endpoint and conversation on first sighting when a rule matches.

use crate::{Core, CoreError, RoutingConfig};
use wuphf_model::{
    AddressSet, Alias, ChatAddress, ChatRef, ConnectorId, Conversation, ConversationId, Endpoint,
    EndpointId, RuleEnd, RuleId, RuleKind, SourceAddress,
};
use wuphf_storage::{Storage, StorageError, TopologyStore};

impl<S> Core<S>
where
    S: Storage,
{
    /// Resolve a raw inbound address to its endpoint, lazily minting the
    /// endpoint and conversation on first sighting when a rule matches. Returns `None` when the address matches no rule, or matches
    /// only a many-to-one destination with no provisioned thread (an unmapped
    /// topic in the container).
    pub(crate) async fn resolve_origin(
        &self,
        routing: &RoutingConfig,
        source: &SourceAddress,
    ) -> Result<Option<EndpointId>, CoreError> {
        if let Some(endpoint) = self
            .storage
            .resolve_endpoint(&source.connector_id, &source.address)
            .await?
        {
            return self.still_covered(routing, endpoint, source).await;
        }

        // Rules are matched against every address form the platform has
        // reported for this chat, not just the one the event arrived under: a
        // chat list written with a WhatsApp phone JID has to select — or
        // exclude — the same chat when it arrives addressed by LID.
        let addresses = self
            .address_set(&source.connector_id, &source.address)
            .await?;
        let Some(matched) = routing.match_source(&source.connector_id, &addresses) else {
            return Ok(None);
        };
        let rule = matched.rule.clone();
        let origin_end = matched.origin;
        // A first sighting arriving on a many-to-one destination is a message
        // in a thread no conversation has claimed yet: conversations are minted
        // from the source end, so there is nothing to bind it to.
        if origin_end == RuleEnd::Destination && matches!(rule.flow, RuleKind::ManyToOne { .. }) {
            return Ok(None);
        }

        // The source end owns conversation identity: a one-to-one rule is keyed
        // by its fixed source chat, a many-to-one rule by the actual source
        // chat this event arrived on.
        let identity_address = match &rule.flow {
            RuleKind::OneToOne { source_chat, .. } => source_chat.clone(),
            RuleKind::ManyToOne { .. } => source.address.clone(),
        };
        let identity_connector = rule.flow.connector(RuleEnd::Source).clone();

        let conversation = self
            .ensure_conversation(&rule.id, identity_connector, &identity_address)
            .await?;

        let mut origin = None;
        for end in [RuleEnd::Source, RuleEnd::Destination] {
            let endpoint = self
                .ensure_end_endpoint(routing, &conversation, &rule.flow, end, &identity_address)
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
    /// Endpoints in a conversation with no rule are exempt: those are the
    /// standalone chats an owner message mints outside every rule, and they
    /// have to stay resolvable so their echoes are recognized.
    async fn still_covered(
        &self,
        routing: &RoutingConfig,
        endpoint_id: EndpointId,
        source: &SourceAddress,
    ) -> Result<Option<EndpointId>, CoreError> {
        let Some(conversation) = self.storage.conversation_for_endpoint(endpoint_id).await? else {
            return Ok(Some(endpoint_id));
        };
        let Some(rule_id) = &conversation.rule_id else {
            return Ok(Some(endpoint_id));
        };
        // A conversation whose rule is gone from the config is no longer
        // covered by anything, so it stops replicating.
        let Some(rule) = routing.rule(rule_id) else {
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
    /// has reported (a chain, since a chat can migrate more than once). See
    /// [`address_forms`].
    pub(crate) async fn address_set(
        &self,
        connector_id: &ConnectorId,
        address: &ChatAddress,
    ) -> Result<AddressSet, CoreError> {
        Ok(address_forms(&*self.storage, connector_id, address).await?)
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

    /// Get the conversation for this rule's identity address, creating it (with
    /// no endpoints yet) on first sighting.
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
        let conversation = Conversation {
            id: ConversationId::new(),
            rule_id: Some(rule_id.clone()),
            source: ChatRef {
                connector_id: identity_connector,
                address: identity_address.clone(),
            },
            endpoints: Vec::new(),
            title: None,
        };
        self.storage.put_conversation(conversation.clone()).await?;
        Ok(conversation)
    }

    /// Get or mint the endpoint for one end of a rule's flow, joining it to the
    /// conversation. Fixed chats and many-to-one sources record a canonical
    /// alias; a many-to-one destination gets no alias until its thread is
    /// provisioned, so it is found through the conversation instead.
    async fn ensure_end_endpoint(
        &self,
        routing: &RoutingConfig,
        conversation: &Conversation,
        flow: &RuleKind,
        end: RuleEnd,
        identity_address: &ChatAddress,
    ) -> Result<EndpointId, CoreError> {
        let connector = flow.connector(end).clone();
        let (address, with_alias) = match (flow, end) {
            (RuleKind::OneToOne { source_chat, .. }, RuleEnd::Source) => {
                (source_chat.clone(), true)
            }
            (
                RuleKind::OneToOne {
                    destination_chat, ..
                },
                RuleEnd::Destination,
            ) => (destination_chat.clone(), true),
            (RuleKind::ManyToOne { .. }, RuleEnd::Source) => (identity_address.clone(), true),
            (
                RuleKind::ManyToOne {
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
            .endpoint_in_conversation(conversation.id, &connector)
            .await?
        {
            return Ok(endpoint);
        }

        let info = routing
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
        self.storage
            .add_conversation_endpoint(conversation.id, endpoint.id)
            .await?;
        Ok(endpoint.id)
    }

    /// The conversation's endpoint on a connector, if it has one. Reads the
    /// conversation afresh: the caller's copy is a snapshot taken before
    /// endpoints were added earlier in the same resolution.
    async fn endpoint_in_conversation(
        &self,
        conversation_id: ConversationId,
        connector: &ConnectorId,
    ) -> Result<Option<EndpointId>, CoreError> {
        let conversation = self
            .storage
            .conversation(conversation_id)
            .await?
            .ok_or(StorageError::ConversationNotFound(conversation_id))?;
        for endpoint_id in conversation.endpoints {
            let endpoint = self.storage.endpoint(endpoint_id).await?;
            if endpoint.connector_id == *connector {
                return Ok(Some(endpoint_id));
            }
        }
        Ok(None)
    }
}

/// Every platform address known to name the chat at `address` on a connector:
/// `address` itself plus the transitive closure of the address links the
/// platform has reported. Walked breadth-first over a visited set, so a cycle —
/// links are stored symmetrically, so every link is one — terminates. Shared by
/// rule matching and by the timer lookup on sends.
pub(crate) async fn address_forms<S: TopologyStore>(
    storage: &S,
    connector_id: &ConnectorId,
    address: &ChatAddress,
) -> Result<AddressSet, StorageError> {
    let mut addresses = AddressSet::single(address.clone());
    let mut frontier = vec![address.clone()];
    while let Some(address) = frontier.pop() {
        for linked in storage.linked_addresses(connector_id, &address).await? {
            if addresses.iter().any(|known| *known == linked) {
                continue;
            }
            addresses.insert(linked.clone());
            frontier.push(linked);
        }
    }
    Ok(addresses)
}
