//! Dispatch settlement: record sent copies, acknowledge or dead-letter
//! dispatches, persist provisioned threads, replay stashed messages, and apply
//! identity migrations.

use crate::{Core, CoreError};
use wuphf_model::{
    Alias, ConnectorId, DispatchAcknowledgement, DispatchId, EndpointId, MessageCopy, MessageId,
    PlatformAddress, StoredDispatch,
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
        platform_message_id: impl Into<String>,
    ) -> Result<(), CoreError> {
        self.storage
            .record_sent_copy(MessageCopy {
                message_id,
                endpoint_id: target_endpoint,
                platform_message_id: platform_message_id.into(),
            })
            .await?;
        Ok(())
    }

    pub async fn acknowledge_dispatch(
        &self,
        acknowledgement: DispatchAcknowledgement,
    ) -> Result<Vec<StoredDispatch>, CoreError> {
        let dispatch = self.storage.dispatch(acknowledgement.dispatch_id).await?;
        if let (Some(thread_id), Some(dispatch)) =
            (acknowledgement.thread_id.clone(), dispatch.as_ref())
        {
            self.persist_provisioned_thread(dispatch, thread_id).await?;
        }
        let mut dispatches = self
            .storage
            .acknowledge_dispatch(
                acknowledgement.dispatch_id,
                acknowledgement.platform_message_id,
            )
            .await?;
        if let Some(dispatch) = dispatch {
            dispatches.extend(self.resume_provisioned(&dispatch).await?);
        }
        Ok(dispatches)
    }

    /// Settle one provisioning dispatch (acknowledged or failed) against its
    /// conversation. Once no provisioning dispatch remains outstanding the
    /// mirror thread is established (or provisioning gave up), so any stashed
    /// messages are replayed and their resulting dispatches returned.
    pub(crate) async fn resume_provisioned(
        &self,
        dispatch: &StoredDispatch,
    ) -> Result<Vec<StoredDispatch>, CoreError> {
        let replica_set_id = dispatch.command.message_replica_set_id();
        let stash = {
            let mut provisioning = self.provisioning.lock().unwrap();
            let Some(state) = provisioning.get_mut(&replica_set_id) else {
                return Ok(Vec::new());
            };
            state.pending.remove(&dispatch.id);
            if !state.pending.is_empty() {
                return Ok(Vec::new());
            }
            provisioning
                .remove(&replica_set_id)
                .map(|state| state.stash)
                .unwrap_or_default()
        };
        let mut dispatches = Vec::new();
        for stashed in stash {
            let (_, routed) = self
                .route_and_record(
                    &stashed.message,
                    stashed.origin_endpoint,
                    &stashed.provision_title,
                )
                .await?;
            dispatches.extend(routed);
        }
        Ok(dispatches)
    }

    /// Record the thread a mirror dispatch created: stamp it onto the endpoint,
    /// register an alias so replies typed into the topic resolve back, and map
    /// the conversation to the thread.
    async fn persist_provisioned_thread(
        &self,
        dispatch: &StoredDispatch,
        thread_id: String,
    ) -> Result<(), CoreError> {
        let connector = dispatch.command.target_connector.clone();
        let container = match &dispatch.command.target.provision_thread {
            Some(provision) => provision.container.clone(),
            None => dispatch.command.target.address.chat_id.clone(),
        };

        let mut endpoint = self
            .storage
            .endpoint(dispatch.command.target_endpoint)
            .await?;
        endpoint.thread_id = Some(thread_id.clone());
        self.storage.put_endpoint(endpoint.clone()).await?;
        self.storage
            .put_alias(Alias {
                connector_id: connector.clone(),
                address: endpoint.address(),
                endpoint_id: endpoint.id,
                is_canonical: true,
            })
            .await?;

        let replica_set_id = dispatch.command.message_replica_set_id();
        if let Some(conversation) = self
            .storage
            .conversation_for_replica_set(replica_set_id)
            .await?
        {
            self.storage
                .put_thread_mapping(wuphf_model::ThreadMapping {
                    conversation_id: conversation.id,
                    connector_id: connector,
                    container,
                    thread_id,
                })
                .await?;
        }
        Ok(())
    }

    /// Handle a platform identity migration: bind the new address to the same
    /// endpoint as the old one and promote it to canonical for outbound sends.
    /// The old alias stays resolvable for inbound. If the old address was never
    /// seen, there is nothing to remap and a later first sighting mints fresh.
    pub async fn migrate_identity(
        &self,
        connector_id: ConnectorId,
        old: PlatformAddress,
        new: PlatformAddress,
    ) -> Result<bool, CoreError> {
        let Some(endpoint_id) = self.storage.resolve_endpoint(&connector_id, &old).await? else {
            return Ok(false);
        };
        self.storage
            .put_alias(Alias {
                connector_id,
                address: new,
                endpoint_id,
                is_canonical: true,
            })
            .await?;
        Ok(true)
    }

    pub async fn pending_dispatches(&self) -> Result<Vec<StoredDispatch>, CoreError> {
        Ok(self.storage.pending_dispatches().await?)
    }

    /// Dead-letter a dispatch the connector reported as permanently undeliverable.
    /// It leaves the pending queue without recording a sent copy, so it is never
    /// redelivered and dependent replies are not released against a phantom send.
    pub async fn fail_dispatch(
        &self,
        dispatch_id: DispatchId,
        reason: String,
    ) -> Result<Vec<StoredDispatch>, CoreError> {
        let dispatch = self.storage.dispatch(dispatch_id).await?;
        self.storage.fail_dispatch(dispatch_id, reason).await?;
        // A failed provisioning dispatch must still release the stash, or the
        // held messages would never send. Replaying them re-attempts the
        // provision with the next message rather than stranding the conversation.
        match dispatch {
            Some(dispatch) => self.resume_provisioned(&dispatch).await,
            None => Ok(Vec::new()),
        }
    }
}
