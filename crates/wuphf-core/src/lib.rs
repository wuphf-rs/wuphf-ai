use std::sync::Arc;
use thiserror::Error;
use wuphf_model::{
    DedupKey, DispatchAcknowledgement, EndpointId, EventPayload, MediaId, Message, MessageCopy,
    MessageId, NormalizedEvent, StoredDispatch,
};
use wuphf_router::{Router, RouterError, RoutingOutcome};
use wuphf_storage::{Storage, StorageError};

pub struct Core<S> {
    storage: Arc<S>,
    router: Router<S>,
}

impl<S> Core<S>
where
    S: Storage,
{
    pub fn new(storage: Arc<S>) -> Self {
        Self {
            router: Router::new(Arc::clone(&storage)),
            storage,
        }
    }

    pub async fn ingest(&self, event: NormalizedEvent) -> Result<IngestOutcome, CoreError> {
        let origin = self.storage.endpoint(event.origin_endpoint).await?;
        let group = self.storage.group(event.group_id).await?;
        if !group.endpoints.contains(&event.origin_endpoint) {
            return Err(CoreError::OriginOutsideGroup(event.origin_endpoint));
        }

        let EventPayload::MessageCreate(inbound) = event.payload else {
            return Err(CoreError::UnsupportedEvent);
        };

        if self
            .storage
            .is_sent_copy(event.origin_endpoint, &inbound.platform_message_id)
            .await?
        {
            return Ok(IngestOutcome::Ignored(IgnoreReason::SentMessageEcho));
        }

        let reply_to = match inbound.reply_to_platform_message_id.as_deref() {
            Some(platform_message_id) => Some(
                self.storage
                    .message_id_for_copy(event.origin_endpoint, platform_message_id)
                    .await?
                    .ok_or_else(|| CoreError::UnknownReplyTarget(platform_message_id.to_owned()))?,
            ),
            None => None,
        };

        for attachment in &inbound.attachments {
            let media = self
                .storage
                .media(attachment.media_id)
                .await?
                .ok_or(CoreError::UnknownMedia(attachment.media_id))?;
            if media.attachment != *attachment {
                return Err(CoreError::MediaMetadataMismatch(attachment.media_id));
            }
        }

        let dedup_key = DedupKey {
            platform: origin.platform,
            chat_id: origin.chat_id,
            platform_message_id: inbound.platform_message_id.clone(),
        };
        if !self.storage.claim_event(dedup_key).await? {
            return Ok(IngestOutcome::Ignored(IgnoreReason::DuplicateEvent));
        }

        let message = Message {
            id: MessageId::new(),
            group_id: event.group_id,
            sender: inbound.sender,
            participant: inbound.participant,
            text: inbound.text,
            attachments: inbound.attachments,
            reply_to,
            timestamp_ms: inbound.timestamp_ms,
        };
        self.storage.put_message(message.clone()).await?;
        self.storage
            .put_received_copy(MessageCopy {
                message_id: message.id,
                endpoint_id: event.origin_endpoint,
                platform_message_id: inbound.platform_message_id,
            })
            .await?;

        let routing = self.router.route(&message, event.origin_endpoint).await?;
        let mut dispatches = Vec::new();
        for command in routing.dispatches.iter().cloned() {
            dispatches.push(self.storage.enqueue_dispatch(command, None).await?);
        }
        for blocked in routing.blocked.iter().cloned() {
            self.storage
                .enqueue_dispatch(blocked.command, Some(blocked.waiting_for_message))
                .await?;
        }
        Ok(IngestOutcome::Routed {
            message_id: message.id,
            routing,
            dispatches,
        })
    }

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
        Ok(self
            .storage
            .acknowledge_dispatch(
                acknowledgement.dispatch_id,
                acknowledgement.platform_message_id,
            )
            .await?)
    }

    pub async fn pending_dispatches(&self) -> Result<Vec<StoredDispatch>, CoreError> {
        Ok(self.storage.pending_dispatches().await?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    Routed {
        message_id: MessageId,
        routing: RoutingOutcome,
        dispatches: Vec<StoredDispatch>,
    },
    Ignored(IgnoreReason),
}

impl IngestOutcome {
    pub fn dispatches(&self) -> &[StoredDispatch] {
        match self {
            Self::Routed { dispatches, .. } => dispatches,
            Self::Ignored(_) => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgnoreReason {
    DuplicateEvent,
    SentMessageEcho,
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Router(#[from] RouterError),
    #[error("origin endpoint {0} is not a member of the event replication group")]
    OriginOutsideGroup(EndpointId),
    #[error("only message create ingestion is implemented in the initial runtime")]
    UnsupportedEvent,
    #[error("reply target platform message {0} is unknown at the origin endpoint")]
    UnknownReplyTarget(String),
    #[error("attachment media {0} is not uploaded to the core")]
    UnknownMedia(MediaId),
    #[error("attachment metadata for media {0} differs from the core record")]
    MediaMetadataMismatch(MediaId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{
        Attachment, AttachmentKind, DispatchPayload, Endpoint, GroupId, InboundMessage, MediaBlob,
        MediaId, NormalizedEvent, Platform, ReplicationGroup,
    };
    use wuphf_storage::InMemoryStorage;

    struct Fixture {
        core: Core<InMemoryStorage>,
        group_id: GroupId,
        whatsapp: EndpointId,
        telegram: EndpointId,
    }

    async fn fixture() -> Fixture {
        let storage = Arc::new(InMemoryStorage::default());
        let whatsapp = EndpointId::new();
        let telegram = EndpointId::new();
        let group_id = GroupId::new();
        storage
            .put_endpoint(Endpoint {
                id: whatsapp,
                platform: Platform::WhatsApp,
                chat_id: "group@s.whatsapp.net".into(),
                thread_id: None,
            })
            .await
            .unwrap();
        storage
            .put_endpoint(Endpoint {
                id: telegram,
                platform: Platform::Telegram,
                chat_id: "-10001".into(),
                thread_id: Some("12".into()),
            })
            .await
            .unwrap();
        storage
            .put_group(ReplicationGroup {
                id: group_id,
                endpoints: vec![whatsapp, telegram],
            })
            .await
            .unwrap();
        Fixture {
            core: Core::new(storage),
            group_id,
            whatsapp,
            telegram,
        }
    }

    fn message_event(
        endpoint: EndpointId,
        group_id: GroupId,
        native_id: &str,
        reply_to: Option<&str>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            origin_endpoint: endpoint,
            group_id,
            payload: EventPayload::MessageCreate(InboundMessage {
                platform_message_id: native_id.into(),
                sender: "alice".into(),
                participant: None,
                text: Some("hello".into()),
                attachments: vec![],
                reply_to_platform_message_id: reply_to.map(str::to_owned),
                timestamp_ms: 1,
            }),
        }
    }

    #[tokio::test]
    async fn fans_out_logical_messages_without_routing_back_to_origin() {
        let fixture = fixture().await;
        let outcome = fixture
            .core
            .ingest(message_event(
                fixture.whatsapp,
                fixture.group_id,
                "wa-1",
                None,
            ))
            .await
            .unwrap();
        let dispatches = outcome.dispatches();
        assert_eq!(dispatches.len(), 1);
        assert_eq!(dispatches[0].command.target_endpoint, fixture.telegram);
    }

    #[tokio::test]
    async fn ignores_duplicates_and_echoes_of_dispatched_copies() {
        let fixture = fixture().await;
        let event = message_event(fixture.whatsapp, fixture.group_id, "wa-1", None);
        let outcome = fixture.core.ingest(event.clone()).await.unwrap();
        assert_eq!(
            fixture.core.ingest(event).await.unwrap(),
            IngestOutcome::Ignored(IgnoreReason::DuplicateEvent)
        );
        let IngestOutcome::Routed { message_id, .. } = outcome else {
            panic!("message should route");
        };
        fixture
            .core
            .record_dispatched_copy(message_id, fixture.telegram, "tg-9")
            .await
            .unwrap();
        assert_eq!(
            fixture
                .core
                .ingest(message_event(
                    fixture.telegram,
                    fixture.group_id,
                    "tg-9",
                    None
                ))
                .await
                .unwrap(),
            IngestOutcome::Ignored(IgnoreReason::SentMessageEcho)
        );
    }

    #[tokio::test]
    async fn maps_replies_through_internal_identity_and_target_copy() {
        let fixture = fixture().await;
        let parent = fixture
            .core
            .ingest(message_event(
                fixture.whatsapp,
                fixture.group_id,
                "wa-parent",
                None,
            ))
            .await
            .unwrap();
        let IngestOutcome::Routed {
            message_id: parent_id,
            ..
        } = parent
        else {
            panic!("parent should route");
        };
        fixture
            .core
            .record_dispatched_copy(parent_id, fixture.telegram, "tg-parent")
            .await
            .unwrap();

        let reply = fixture
            .core
            .ingest(message_event(
                fixture.whatsapp,
                fixture.group_id,
                "wa-reply",
                Some("wa-parent"),
            ))
            .await
            .unwrap();
        let DispatchPayload::MessageCreate {
            message,
            reply_context,
        } = &reply.dispatches()[0].command.payload;
        assert_eq!(message.reply_to, Some(parent_id));
        let reply_context = reply_context.as_ref().expect("reply context resolved");
        assert_eq!(reply_context.message_id, parent_id);
        assert_eq!(reply_context.platform_message_id, "tg-parent");
    }

    #[tokio::test]
    async fn blocks_reply_dispatch_until_target_parent_copy_exists() {
        let fixture = fixture().await;
        fixture
            .core
            .ingest(message_event(
                fixture.whatsapp,
                fixture.group_id,
                "wa-parent",
                None,
            ))
            .await
            .unwrap();

        let reply = fixture
            .core
            .ingest(message_event(
                fixture.whatsapp,
                fixture.group_id,
                "wa-reply",
                Some("wa-parent"),
            ))
            .await
            .unwrap();
        let IngestOutcome::Routed { routing, .. } = reply else {
            panic!("reply should be stored while delivery is blocked");
        };
        assert!(routing.dispatches.is_empty());
        assert_eq!(routing.blocked.len(), 1);
        assert_eq!(routing.blocked[0].command.target_endpoint, fixture.telegram);
    }

    #[tokio::test]
    async fn rejects_unknown_or_modified_media_before_claiming_event() {
        let fixture = fixture().await;
        let attachment = Attachment {
            media_id: MediaId::new(),
            kind: AttachmentKind::Image,
            file_name: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            size_bytes: Some(5),
        };
        let event = media_event(
            fixture.whatsapp,
            fixture.group_id,
            "wa-media",
            attachment.clone(),
        );
        assert!(matches!(
            fixture.core.ingest(event.clone()).await,
            Err(CoreError::UnknownMedia(id)) if id == attachment.media_id
        ));

        fixture
            .core
            .storage
            .put_media(
                MediaBlob {
                    attachment: attachment.clone(),
                    bytes: b"hello".to_vec(),
                },
                1,
            )
            .await
            .unwrap();
        let mut altered = attachment.clone();
        altered.kind = AttachmentKind::File;
        assert!(matches!(
            fixture
                .core
                .ingest(media_event(
                    fixture.whatsapp,
                    fixture.group_id,
                    "wa-media",
                    altered
                ))
                .await,
            Err(CoreError::MediaMetadataMismatch(id)) if id == attachment.media_id
        ));

        assert_eq!(
            fixture.core.ingest(event).await.unwrap().dispatches().len(),
            1
        );
    }

    fn media_event(
        endpoint: EndpointId,
        group_id: GroupId,
        native_id: &str,
        attachment: Attachment,
    ) -> NormalizedEvent {
        let mut event = message_event(endpoint, group_id, native_id, None);
        let EventPayload::MessageCreate(inbound) = &mut event.payload else {
            unreachable!();
        };
        inbound.attachments = vec![attachment];
        event
    }
}
