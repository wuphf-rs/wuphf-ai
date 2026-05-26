use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{DispatchCommand, DispatchPayload, EndpointId, Message, MessageId};
use wuphf_storage::{Storage, StorageError};

pub struct Router<S> {
    storage: Arc<S>,
}

impl<S> Router<S>
where
    S: Storage,
{
    pub fn new(storage: Arc<S>) -> Self {
        Self { storage }
    }

    pub async fn route(
        &self,
        message: &Message,
        origin_endpoint: EndpointId,
    ) -> Result<RoutingOutcome, RouterError> {
        let group = self.storage.group(message.group_id).await?;
        if !group.endpoints.contains(&origin_endpoint) {
            return Err(RouterError::OriginOutsideGroup(origin_endpoint));
        }

        let mut outcome = RoutingOutcome::default();
        for endpoint_id in group
            .endpoints
            .into_iter()
            .filter(|endpoint_id| *endpoint_id != origin_endpoint)
        {
            let native_reply = match message.reply_to {
                Some(parent_id) => {
                    match self
                        .storage
                        .copy_at_endpoint(parent_id, endpoint_id)
                        .await?
                    {
                        Some(copy) => Some(copy.platform_message_id),
                        None => {
                            outcome.blocked.push(BlockedDispatch {
                                command: DispatchCommand {
                                    target_endpoint: endpoint_id,
                                    payload: DispatchPayload::MessageCreate {
                                        message: message.clone(),
                                        reply_to_platform_message_id: None,
                                    },
                                },
                                waiting_for_message: parent_id,
                            });
                            continue;
                        }
                    }
                }
                None => None,
            };

            outcome.dispatches.push(DispatchCommand {
                target_endpoint: endpoint_id,
                payload: DispatchPayload::MessageCreate {
                    message: message.clone(),
                    reply_to_platform_message_id: native_reply,
                },
            });
        }
        Ok(outcome)
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
    #[error("origin endpoint {0} is not a member of the replication group")]
    OriginOutsideGroup(EndpointId),
}
