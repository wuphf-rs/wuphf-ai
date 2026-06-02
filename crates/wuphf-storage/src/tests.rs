use super::*;
use wuphf_model::{Attachment, AttachmentKind, DispatchPayload, MediaBlob, MediaId, Platform};

#[tokio::test]
async fn sqlite_persists_copy_state_and_releases_blocked_reply() {
    let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    let parent_id = MessageId::new();
    let reply_id = MessageId::new();
    let endpoint_id = EndpointId::new();

    let storage = SqliteStorage::connect(&url).await.unwrap();
    storage
        .put_endpoint(Endpoint {
            id: endpoint_id,
            connector_id: ConnectorId("telegram".into()),
            platform: Platform::Telegram,
            chat_id: "-100".into(),
            thread_id: Some("7".into()),
        })
        .await
        .unwrap();
    storage.put_message(message(parent_id)).await.unwrap();
    let parent = storage
        .enqueue_dispatch(command(parent_id, endpoint_id), None)
        .await
        .unwrap();
    storage
        .enqueue_dispatch(command(reply_id, endpoint_id), Some(parent_id))
        .await
        .unwrap();
    let released = storage
        .acknowledge_dispatch(parent.id, "telegram-parent".into())
        .await
        .unwrap();
    assert_eq!(released.len(), 1);
    let DispatchPayload::MessageCreate { reply_context, .. } = &released[0].command.payload else {
        panic!("released dispatch should be a message create");
    };
    let reply_context = reply_context
        .as_ref()
        .expect("reply context set on release");
    assert_eq!(reply_context.message_id, parent_id);
    assert_eq!(reply_context.platform_message_id, "telegram-parent");
    drop(storage);

    let reopened = SqliteStorage::connect(&url).await.unwrap();
    assert_eq!(reopened.pending_dispatches().await.unwrap(), released);
    assert!(
        reopened
            .is_sent_copy(endpoint_id, "telegram-parent")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn sqlite_persists_media_blobs() {
    let path = std::env::temp_dir().join(format!("wuphf-media-{}.sqlite", MediaId::new()));
    let url = format!("sqlite://{}", path.display());
    let attachment = Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::Image,
        file_name: Some("photo.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        size_bytes: Some(5),
    };
    let storage = SqliteStorage::connect(&url).await.unwrap();
    storage
        .put_media(
            MediaBlob {
                attachment: attachment.clone(),
                bytes: b"hello".to_vec(),
            },
            10,
        )
        .await
        .unwrap();
    drop(storage);

    let reopened = SqliteStorage::connect(&url).await.unwrap();
    let media = reopened.media(attachment.media_id).await.unwrap().unwrap();
    assert_eq!(media.attachment, attachment);
    assert_eq!(media.bytes, b"hello");
}

#[tokio::test]
async fn sqlite_prunes_only_expired_unreferenced_media() {
    let path = std::env::temp_dir().join(format!("wuphf-prune-{}.sqlite", MediaId::new()));
    let url = format!("sqlite://{}", path.display());
    let storage = SqliteStorage::connect(&url).await.unwrap();
    let referenced = attachment();
    let orphan = attachment();
    for attachment in [&referenced, &orphan] {
        storage
            .put_media(
                MediaBlob {
                    attachment: attachment.clone(),
                    bytes: b"old".to_vec(),
                },
                10,
            )
            .await
            .unwrap();
    }
    storage
        .put_message(Message {
            id: MessageId::new(),
            replica_set_id: ReplicaSetId::new(),
            sender: "test".into(),
            participant: None,
            sender_name: None,
            text: None,
            attachments: vec![referenced.clone()],
            reply_to: None,
            timestamp_ms: 0,
            deleted: false,
        })
        .await
        .unwrap();

    assert_eq!(
        storage.prune_unreferenced_media_before(20).await.unwrap(),
        1
    );
    assert!(storage.media(referenced.media_id).await.unwrap().is_some());
    assert!(storage.media(orphan.media_id).await.unwrap().is_none());
}

fn attachment() -> Attachment {
    Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::File,
        file_name: Some("file.bin".into()),
        mime_type: None,
        size_bytes: Some(3),
    }
}

fn message(message_id: MessageId) -> Message {
    Message {
        id: message_id,
        replica_set_id: ReplicaSetId::new(),
        sender: "test".into(),
        participant: None,
        sender_name: None,
        text: Some("test".into()),
        attachments: vec![],
        reply_to: None,
        timestamp_ms: 0,
        deleted: false,
    }
}

fn command(message_id: MessageId, endpoint_id: EndpointId) -> DispatchCommand {
    DispatchCommand {
        target_endpoint: endpoint_id,
        target_connector: ConnectorId("telegram".into()),
        target: wuphf_model::TargetAddress {
            address: wuphf_model::PlatformAddress::new("-100", Some("7".into())),
            provision_thread: None,
        },
        payload: DispatchPayload::MessageCreate {
            message: message(message_id),
            reply_context: None,
        },
    }
}
