//! Dispatch settlement: record sent copies, acknowledge or dead-letter
//! dispatches, persist provisioned threads, replay stashed messages, and apply
//! identity migrations.

use crate::{Core, CoreError, RoutingConfig};
use std::sync::Arc;
use wuphf_model::{
    Action, AddressLink, Alias, ChatAddress, ConnectorId, ConversationId, Dispatch, DispatchAck,
    DispatchId, EndpointId, MessageCopy, MessageId,
};
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    pub async fn record_dispatched_copy(
        &self,
        message_id: MessageId,
        target_endpoint: EndpointId,
        native_id: impl Into<String>,
    ) -> Result<(), CoreError> {
        self.storage
            .record_sent_copy(MessageCopy {
                message_id,
                endpoint_id: target_endpoint,
                native_id: native_id.into(),
            })
            .await?;
        Ok(())
    }

    pub async fn acknowledge_dispatch(
        &self,
        acknowledgement: DispatchAck,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let routing = self.routing.routing();
        let dispatch = self.storage.dispatch(acknowledgement.dispatch_id).await?;
        if let (Some(thread_id), Some(dispatch)) =
            (acknowledgement.thread_id.clone(), dispatch.as_ref())
        {
            self.persist_provisioned_thread(dispatch, thread_id).await?;
        }
        let mut dispatches = self
            .storage
            .acknowledge_dispatch(acknowledgement.dispatch_id, acknowledgement.native_id)
            .await?;
        if let Some(dispatch) = dispatch {
            if let Some(ack) = self.delivery_ack_for(&dispatch).await? {
                dispatches.push(ack);
            }
            dispatches.extend(self.resume_provisioned(&routing, &dispatch).await?);
        }
        Ok(dispatches)
    }

    /// Build a delivery-ack dispatch for a just-acknowledged mirror create: a
    /// signal to the message's origin endpoint that the copy reached a mirror.
    /// Only message creates trigger it (an edit/delete/reaction ack is not a new
    /// delivery), and only when the origin copy is known. The connector decides
    /// how to surface it (Telegram reacts 👍; sources ignore it). Multiple
    /// mirrors each emit one; the signal is idempotent.
    async fn delivery_ack_for(&self, dispatch: &Dispatch) -> Result<Option<Dispatch>, CoreError> {
        if !matches!(dispatch.action, wuphf_model::Action::Send { .. }) {
            return Ok(None);
        }
        let Some(message_id) = dispatch.message_id() else {
            return Ok(None);
        };
        let Some(origin) = self.storage.received_copy(message_id).await? else {
            return Ok(None);
        };
        let endpoint = self.storage.endpoint(origin.endpoint_id).await?;
        let ack = Dispatch::new(
            wuphf_model::Target {
                endpoint_id: endpoint.id,
                connector_id: endpoint.connector_id.clone(),
                address: endpoint.address(),
                create_thread: None,
            },
            dispatch.conversation_id,
            wuphf_model::Action::Receipt {
                message_id,
                native_id: origin.native_id,
                kind: wuphf_model::ReceiptKind::Delivered,
            },
            None,
        );
        self.storage.enqueue_dispatch(&ack).await?;
        Ok(Some(ack))
    }

    /// Settle one provisioning dispatch (acknowledged or failed) against its
    /// conversation. Once no provisioning dispatch remains outstanding the
    /// mirror thread is established (or provisioning gave up), so any stashed
    /// messages are replayed and their resulting dispatches returned.
    pub(crate) async fn resume_provisioned(
        &self,
        routing: &Arc<RoutingConfig>,
        dispatch: &Dispatch,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let conversation_id = dispatch.conversation_id;
        {
            let mut provisioning = self.provisioning.lock().unwrap();
            let Some(state) = provisioning.get_mut(&conversation_id) else {
                return Ok(Vec::new());
            };
            state.pending.remove(&dispatch.id);
            if !state.pending.is_empty() {
                return Ok(Vec::new());
            }
            provisioning.remove(&conversation_id);
        }
        self.replay_stash(routing, conversation_id).await
    }

    /// Drain the durable provisioning stash for a conversation and re-route each
    /// held message, returning the resulting dispatches. Replay re-attempts
    /// provisioning if the thread still does not exist (a failed provision), so
    /// the conversation is not stranded.
    ///
    /// Only one replayed message may provision: once one has started a new
    /// provision, the rest go back into the stash in order and wait for it,
    /// exactly as messages arriving during a provision do. Without this, a
    /// failed provision with several messages held would create one thread
    /// per message.
    pub(crate) async fn replay_stash(
        &self,
        routing: &Arc<RoutingConfig>,
        conversation_id: ConversationId,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let stash = self
            .storage
            .drain_provisioning_stash(conversation_id)
            .await?;
        let mut dispatches = Vec::new();
        for (message, origin_endpoint, provision_title) in stash {
            if self.is_provisioning(conversation_id) {
                self.storage
                    .stash_provisioning_message(
                        conversation_id,
                        message.id,
                        origin_endpoint,
                        &provision_title,
                    )
                    .await?;
                continue;
            }
            let (routed, _) = self
                .route_and_record(routing, &message, origin_endpoint, &provision_title)
                .await?;
            dispatches.extend(routed);
        }
        Ok(dispatches)
    }

    /// Whether a provision for the conversation's mirror thread is in flight.
    pub(crate) fn is_provisioning(&self, conversation_id: ConversationId) -> bool {
        self.provisioning
            .lock()
            .unwrap()
            .get(&conversation_id)
            .is_some_and(|state| !state.pending.is_empty())
    }

    /// Durably stash a message deferred behind in-flight provisioning, closing
    /// the race where the provision settles between the caller's pending-check
    /// and this write: if it settled, the settle already drained the stash and
    /// will not run again, so replay the now-orphaned row immediately rather than
    /// stranding it. Returns the dispatches to emit — the replayed ones on that
    /// race, otherwise empty (the message waits for the provision to settle).
    pub(crate) async fn stash_deferred(
        &self,
        routing: &Arc<RoutingConfig>,
        conversation_id: ConversationId,
        message_id: MessageId,
        origin_endpoint: EndpointId,
        provision_title: &str,
    ) -> Result<Vec<Dispatch>, CoreError> {
        self.storage
            .stash_provisioning_message(
                conversation_id,
                message_id,
                origin_endpoint,
                provision_title,
            )
            .await?;
        let settled = {
            let provisioning = self.provisioning.lock().unwrap();
            !provisioning.contains_key(&conversation_id)
        };
        if settled {
            self.replay_stash(routing, conversation_id).await
        } else {
            Ok(Vec::new())
        }
    }

    /// Rebuild the provisioning cache from storage after a restart: every pending
    /// dispatch that provisions a thread re-registers its conversation, so a later
    /// acknowledgement finds the set and drains its durable stash. Any stashed
    /// conversation with no such dispatch left — its provision was acknowledged or
    /// failed in a previous run, or was lost — is drained and replayed
    /// immediately, and the resulting dispatches returned for delivery.
    pub async fn recover_provisioning(&self) -> Result<Vec<Dispatch>, CoreError> {
        let routing = self.routing.routing();
        let mut pending = self.storage.pending_dispatches().await?;
        // A held thread creation is still its conversation's provisioning
        // (plan 079): its stash waits for the release.
        pending.extend(
            self.storage
                .held_dispatches()
                .await?
                .into_iter()
                .map(|held| held.dispatch),
        );
        {
            let mut provisioning = self.provisioning.lock().unwrap();
            for dispatch in &pending {
                if dispatch.to.create_thread.is_some() {
                    provisioning
                        .entry(dispatch.conversation_id)
                        .or_default()
                        .pending
                        .insert(dispatch.id);
                }
            }
        }
        let mut dispatches = Vec::new();
        for conversation_id in self.storage.stashed_conversations().await? {
            let outstanding = {
                let provisioning = self.provisioning.lock().unwrap();
                provisioning
                    .get(&conversation_id)
                    .is_some_and(|state| !state.pending.is_empty())
            };
            if !outstanding {
                dispatches.extend(self.replay_stash(&routing, conversation_id).await?);
            }
        }
        Ok(dispatches)
    }

    /// Record the thread a mirror dispatch created: stamp it onto the endpoint,
    /// which is where routing reads it from, and register an alias so replies
    /// typed into the topic resolve back.
    async fn persist_provisioned_thread(
        &self,
        dispatch: &Dispatch,
        thread_id: String,
    ) -> Result<(), CoreError> {
        let mut endpoint = self.storage.endpoint(dispatch.to.endpoint_id).await?;
        endpoint.thread_id = Some(thread_id);
        self.storage.put_endpoint(endpoint.clone()).await?;
        self.storage
            .put_alias(Alias {
                connector_id: dispatch.to.connector_id.clone(),
                address: endpoint.address(),
                endpoint_id: endpoint.id,
                is_canonical: true,
            })
            .await?;
        Ok(())
    }

    /// Handle a platform identity migration: bind the new address to the same
    /// endpoint as the old one and promote it to canonical for outbound sends.
    /// The old alias stays resolvable for inbound.
    ///
    /// The equivalence itself is recorded either way, before any endpoint is
    /// considered. A migration for a chat that has never been routed still
    /// teaches the core that the two addresses name one chat, which is what
    /// lets a rule's chat list recognize the chat whichever form the user wrote
    /// — including a chat the list is there to exclude. Returns whether an
    /// existing endpoint was rebound.
    ///
    /// A platform can also report a form the core has not seen for a chat it
    /// already routes under the other one — WhatsApp does this when the phone
    /// number behind an already-LID-addressed chat is learned mid-run. There is
    /// nothing to migrate then, but the newly learned form is still bound to
    /// the endpoint (non-canonically, leaving outbound addressing alone) so an
    /// event arriving under it joins the existing conversation instead of
    /// minting a second one with its own mirror thread.
    pub async fn migrate_identity(
        &self,
        connector_id: ConnectorId,
        old: ChatAddress,
        new: ChatAddress,
    ) -> Result<bool, CoreError> {
        self.storage
            .put_address_link(AddressLink {
                connector_id: connector_id.clone(),
                address: old.clone(),
                linked_address: new.clone(),
            })
            .await?;
        // Heal the observation log too. It is written before routing so it can
        // capture chats no rule matches, which means it never passes through
        // the alias layer; without this the rows logged under the old address
        // would stay a separate conversation in the observer forever.
        self.storage
            .rename_observed_chat(&connector_id, &old.chat_id, &new.chat_id)
            .await?;
        let (address, endpoint_id, is_canonical) =
            match self.storage.resolve_endpoint(&connector_id, &old).await? {
                Some(endpoint_id) => (new, endpoint_id, true),
                None => match self.storage.resolve_endpoint(&connector_id, &new).await? {
                    Some(endpoint_id) => (old, endpoint_id, false),
                    None => return Ok(false),
                },
            };
        self.storage
            .put_alias(Alias {
                connector_id,
                address,
                endpoint_id,
                is_canonical,
            })
            .await?;
        Ok(is_canonical)
    }

    pub async fn pending_dispatches(&self) -> Result<Vec<Dispatch>, CoreError> {
        Ok(self.storage.pending_dispatches().await?)
    }

    /// Dead-letter a dispatch the connector reported as permanently undeliverable.
    /// It leaves the pending queue without recording a sent copy, so it is never
    /// redelivered and dependent replies are not released against a phantom send.
    pub async fn fail_dispatch(
        &self,
        dispatch_id: DispatchId,
        reason: String,
    ) -> Result<Vec<Dispatch>, CoreError> {
        let routing = self.routing.routing();
        let dispatch = self.storage.dispatch(dispatch_id).await?;
        self.storage.fail_dispatch(dispatch_id, reason).await?;
        // An owner's reaction that never landed is not shown there, so the
        // reaction stored as the bridge's at the target goes too: otherwise
        // the same reaction from the phone would later read as its echo.
        if let Some(Dispatch {
            to,
            action:
                Action::React {
                    message_id,
                    by_owner: true,
                    ..
                },
            ..
        }) = &dispatch
        {
            self.storage
                .set_reaction(*message_id, to.endpoint_id, "", None)
                .await?;
        }
        // A failed provisioning dispatch must still release the stash, or the
        // held messages would never send. Replaying them re-attempts the
        // provision with the next message rather than stranding the conversation.
        match dispatch {
            Some(dispatch) => self.resume_provisioned(&routing, &dispatch).await,
            None => Ok(Vec::new()),
        }
    }
}
