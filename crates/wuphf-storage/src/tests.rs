use super::*;
use wuphf_model::{
    Action, Attachment, AttachmentKind, DedupKey, DispatchState, MediaBlob, MediaId, MessageCopy,
    ObservedDirection, ObservedMessage, Platform,
};

#[tokio::test]
async fn memory_persists_copy_state_and_releases_blocked_reply() {
    copy_state_and_blocked_reply_released(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_persists_copy_state_and_releases_blocked_reply() {
    let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    let storage = SqliteStorage::connect(&url).await.unwrap();
    let (released, endpoint_id) = copy_state_and_blocked_reply_released(storage).await;

    // sqlite-only: the released state and recorded copy survive a reopen.
    let reopened = SqliteStorage::connect(&url).await.unwrap();
    assert_eq!(reopened.pending_dispatches().await.unwrap(), released);
    assert!(
        reopened
            .is_sent_copy(endpoint_id, "telegram-parent")
            .await
            .unwrap()
    );
}

/// Acknowledging a create releases the reply blocked behind it with the parent's
/// native id bound as reply context, and records the create as a sent copy.
/// Returns the released dispatches and the endpoint so a persistence-capable
/// backend can assert they survive a reopen.
async fn copy_state_and_blocked_reply_released<S: Storage>(
    storage: S,
) -> (Vec<Dispatch>, EndpointId) {
    let parent_id = MessageId::new();
    let reply_id = MessageId::new();
    let endpoint_id = EndpointId::new();

    storage.put_endpoint(endpoint(endpoint_id)).await.unwrap();
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
    let Action::Send {
        reply: reply_context,
        ..
    } = &released[0].command.payload
    else {
        panic!("released dispatch should be a message create");
    };
    let reply_context = reply_context
        .as_ref()
        .expect("reply context set on release");
    assert_eq!(reply_context.message_id, parent_id);
    assert_eq!(reply_context.native_id, "telegram-parent");
    assert!(
        storage
            .is_sent_copy(endpoint_id, "telegram-parent")
            .await
            .unwrap()
    );
    (released, endpoint_id)
}

#[tokio::test]
async fn memory_persists_media_blobs() {
    persists_media_blobs(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_persists_media_blobs() {
    let path = std::env::temp_dir().join(format!("wuphf-media-{}.sqlite", MediaId::new()));
    let url = format!("sqlite://{}", path.display());
    let storage = SqliteStorage::connect(&url).await.unwrap();
    let stored = persists_media_blobs(storage).await;

    // sqlite-only: the blob survives a reopen.
    let reopened = SqliteStorage::connect(&url).await.unwrap();
    let media = reopened.media(stored.media_id).await.unwrap().unwrap();
    assert_eq!(media.attachment, stored);
    assert_eq!(media.bytes, b"hello");
}

/// Storing a media blob makes it readable back with its attachment metadata and
/// bytes intact. Returns the attachment so a persistence-capable backend can
/// assert it survives a reopen.
async fn persists_media_blobs<S: Storage>(storage: S) -> Attachment {
    let attachment = Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::Image,
        file_name: Some("photo.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        size_bytes: Some(5),
        animated: false,
    };
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
    let media = storage.media(attachment.media_id).await.unwrap().unwrap();
    assert_eq!(media.attachment, attachment);
    assert_eq!(media.bytes, b"hello");
    attachment
}

#[tokio::test]
async fn memory_prunes_only_expired_unreferenced_media() {
    prunes_only_expired_unreferenced_media(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_prunes_only_expired_unreferenced_media() {
    let path = std::env::temp_dir().join(format!("wuphf-prune-{}.sqlite", MediaId::new()));
    let url = format!("sqlite://{}", path.display());
    prunes_only_expired_unreferenced_media(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// Pruning drops only media that is both past the cutoff and unreferenced by any
/// stored message; a referenced blob is kept.
async fn prunes_only_expired_unreferenced_media<S: Storage>(storage: S) {
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
            author_id: "test".into(),
            participant: None,
            author_name: None,
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

#[tokio::test]
async fn memory_claim_event_dedups_repeat_events() {
    claim_event_dedups_repeat_events(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_claim_event_dedups_repeat_events() {
    let path = std::env::temp_dir().join(format!("wuphf-dedup-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    claim_event_dedups_repeat_events(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// The first claim of an event key wins; every later claim of the same key is
/// rejected. This is the loop-prevention primitive, so both backends must agree.
async fn claim_event_dedups_repeat_events<S: Storage>(storage: S) {
    let key = DedupKey {
        platform: Platform::WhatsApp,
        chat_id: "chat".into(),
        native_id: "m-1".into(),
    };
    assert!(storage.claim_event(key.clone()).await.unwrap());
    assert!(!storage.claim_event(key.clone()).await.unwrap());
    // A different key is unaffected.
    let other = DedupKey {
        platform: Platform::WhatsApp,
        chat_id: "chat".into(),
        native_id: "m-2".into(),
    };
    assert!(storage.claim_event(other).await.unwrap());
}

#[tokio::test]
async fn memory_sent_copy_detection_and_reverse_lookup() {
    sent_copy_detection_and_reverse_lookup(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_sent_copy_detection_and_reverse_lookup() {
    let path = std::env::temp_dir().join(format!("wuphf-copy-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    sent_copy_detection_and_reverse_lookup(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// A recorded sent copy is detectable as our own echo and resolves back to its
/// message id; an unknown native id matches neither.
async fn sent_copy_detection_and_reverse_lookup<S: Storage>(storage: S) {
    let message_id = MessageId::new();
    let endpoint_id = EndpointId::new();
    storage.put_endpoint(endpoint(endpoint_id)).await.unwrap();
    storage.put_message(message(message_id)).await.unwrap();
    storage
        .record_sent_copy(MessageCopy {
            message_id,
            endpoint_id,
            native_id: "native-1".into(),
        })
        .await
        .unwrap();

    assert!(storage.is_sent_copy(endpoint_id, "native-1").await.unwrap());
    assert!(!storage.is_sent_copy(endpoint_id, "other").await.unwrap());
    assert_eq!(
        storage
            .message_id_for_copy(endpoint_id, "native-1")
            .await
            .unwrap(),
        Some(message_id)
    );
    assert_eq!(
        storage
            .message_id_for_copy(endpoint_id, "other")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn memory_acknowledging_create_binds_blocked_edit() {
    acknowledging_create_binds_blocked_edit(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_acknowledging_create_binds_blocked_edit() {
    let path = std::env::temp_dir().join(format!("wuphf-edit-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    acknowledging_create_binds_blocked_edit(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// An edit blocked behind its message's create is released when that create is
/// acknowledged, with the create's native id bound as the edit's target — the
/// edit-side of `bind_native_parent`.
async fn acknowledging_create_binds_blocked_edit<S: Storage>(storage: S) {
    let message_id = MessageId::new();
    let endpoint_id = EndpointId::new();
    storage.put_endpoint(endpoint(endpoint_id)).await.unwrap();
    storage.put_message(message(message_id)).await.unwrap();
    let create = storage
        .enqueue_dispatch(command(message_id, endpoint_id), None)
        .await
        .unwrap();
    storage
        .enqueue_dispatch(edit_command(message_id, endpoint_id), Some(message_id))
        .await
        .unwrap();

    let released = storage
        .acknowledge_dispatch(create.id, "native-1".into())
        .await
        .unwrap();
    assert_eq!(released.len(), 1);
    let Action::Edit { native_id, .. } = &released[0].command.payload else {
        panic!("released dispatch should be an edit");
    };
    assert_eq!(native_id, "native-1");
}

#[tokio::test]
async fn memory_settling_a_signal_dispatch_records_no_copy() {
    settling_a_signal_dispatch_records_no_copy(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_settling_a_signal_dispatch_records_no_copy() {
    let path = std::env::temp_dir().join(format!("wuphf-signal-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    settling_a_signal_dispatch_records_no_copy(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// Acknowledging a reaction, delivery ack, or seen receipt aimed at a message's
/// origin copy leaves that copy received: recording it as sent would make the
/// bridge treat the user's own message as an echo. Nor may a signal release a
/// dispatch blocked on the message, since no new copy reached the endpoint.
async fn settling_a_signal_dispatch_records_no_copy<S: Storage>(storage: S) {
    let message_id = MessageId::new();
    let endpoint_id = EndpointId::new();
    let replica_set_id = message(message_id).replica_set_id;
    storage.put_endpoint(endpoint(endpoint_id)).await.unwrap();
    storage.put_message(message(message_id)).await.unwrap();
    storage
        .put_received_copy(MessageCopy {
            message_id,
            endpoint_id,
            native_id: "origin-1".into(),
        })
        .await
        .unwrap();
    let blocked = storage
        .enqueue_dispatch(command(MessageId::new(), endpoint_id), Some(message_id))
        .await
        .unwrap();

    let signals = [
        Action::React {
            message_id,
            replica_set_id,
            native_id: "origin-1".into(),
            reactor: "someone".into(),
            reactor_name: None,
            emoji: Some("👍".into()),
        },
        Action::DeliveryAck {
            message_id,
            replica_set_id,
            native_id: "origin-1".into(),
        },
        Action::SeenReceipt {
            message_id,
            replica_set_id,
            native_id: "origin-1".into(),
        },
    ];
    for payload in signals {
        let signal = storage
            .enqueue_dispatch(
                DispatchCommand {
                    payload,
                    ..command(message_id, endpoint_id)
                },
                None,
            )
            .await
            .unwrap();
        let released = storage
            .acknowledge_dispatch(signal.id, "origin-1".into())
            .await
            .unwrap();
        assert!(released.is_empty());
        assert!(!storage.is_sent_copy(endpoint_id, "origin-1").await.unwrap());
    }

    assert_eq!(
        storage
            .message_id_for_copy(endpoint_id, "origin-1")
            .await
            .unwrap(),
        Some(message_id)
    );
    assert!(
        !storage
            .pending_dispatches()
            .await
            .unwrap()
            .iter()
            .any(|dispatch| dispatch.id == blocked.id)
    );
}

#[tokio::test]
async fn memory_fail_dispatch_removes_from_pending() {
    fail_dispatch_removes_from_pending(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_fail_dispatch_removes_from_pending() {
    let path = std::env::temp_dir().join(format!("wuphf-fail-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    fail_dispatch_removes_from_pending(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// A failed dispatch leaves the pending queue for good — a second
/// `pending_dispatches` does not resurrect it — and its state is `Failed`.
async fn fail_dispatch_removes_from_pending<S: Storage>(storage: S) {
    let message_id = MessageId::new();
    let endpoint_id = EndpointId::new();
    storage.put_endpoint(endpoint(endpoint_id)).await.unwrap();
    storage.put_message(message(message_id)).await.unwrap();
    let dispatch = storage
        .enqueue_dispatch(command(message_id, endpoint_id), None)
        .await
        .unwrap();
    assert_eq!(storage.pending_dispatches().await.unwrap().len(), 1);

    storage
        .fail_dispatch(dispatch.id, "invalid target".into())
        .await
        .unwrap();
    assert!(storage.pending_dispatches().await.unwrap().is_empty());
    assert!(storage.pending_dispatches().await.unwrap().is_empty());

    let stored = storage.dispatch(dispatch.id).await.unwrap().unwrap();
    let DispatchState::Failed { reason } = stored.state else {
        panic!("failed dispatch should be in the Failed state");
    };
    assert_eq!(reason, "invalid target");
}

#[tokio::test]
async fn memory_replica_set_for_endpoint_resolves_membership() {
    replica_set_for_endpoint_resolves_membership(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_replica_set_for_endpoint_resolves_membership() {
    let path = std::env::temp_dir().join(format!("wuphf-set-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    replica_set_for_endpoint_resolves_membership(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// `replica_set_for_endpoint` returns the set that contains an endpoint and
/// `None` for an endpoint in no set.
async fn replica_set_for_endpoint_resolves_membership<S: Storage>(storage: S) {
    let endpoint_a = EndpointId::new();
    let endpoint_b = EndpointId::new();
    let set_id = ReplicaSetId::new();
    storage.put_endpoint(endpoint(endpoint_a)).await.unwrap();
    storage.put_endpoint(endpoint(endpoint_b)).await.unwrap();
    storage
        .put_replica_set(ReplicaSet {
            id: set_id,
            endpoints: vec![endpoint_a, endpoint_b],
        })
        .await
        .unwrap();

    assert_eq!(
        storage.replica_set_for_endpoint(endpoint_a).await.unwrap(),
        Some(set_id)
    );
    assert_eq!(
        storage.replica_set_for_endpoint(endpoint_b).await.unwrap(),
        Some(set_id)
    );
    assert_eq!(
        storage
            .replica_set_for_endpoint(EndpointId::new())
            .await
            .unwrap(),
        None
    );
}

fn attachment() -> Attachment {
    Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::File,
        file_name: Some("file.bin".into()),
        mime_type: None,
        size_bytes: Some(3),
        animated: false,
    }
}

fn endpoint(endpoint_id: EndpointId) -> Endpoint {
    Endpoint {
        id: endpoint_id,
        connector_id: ConnectorId("telegram".into()),
        platform: Platform::Telegram,
        chat_id: "-100".into(),
        thread_id: Some("7".into()),
    }
}

fn message(message_id: MessageId) -> Message {
    Message {
        id: message_id,
        replica_set_id: ReplicaSetId::new(),
        author_id: "test".into(),
        participant: None,
        author_name: None,
        text: Some("test".into()),
        attachments: vec![],
        reply_to: None,
        timestamp_ms: 0,
        deleted: false,
    }
}

#[tokio::test]
async fn memory_observation_log_roundtrips() {
    observation_log_roundtrips(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_observation_log_roundtrips() {
    let path = std::env::temp_dir().join(format!("wuphf-observed-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    observation_log_roundtrips(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// Both backends must agree on the observation log: chats ordered by recency,
/// messages paged newest-first, titles retained across a title-less message, and
/// edit/delete markers mutating in place and reporting the updated row.
async fn observation_log_roundtrips<S: Storage>(storage: S) {
    let whatsapp = ConnectorId("whatsapp".into());
    // Two chats: "alice" gets messages at t=100 then t=200; "bob" at t=150.
    storage
        .log_observed(
            observed(&whatsapp, "alice", "a1", 100, Some("first")),
            Some("Alice".into()),
        )
        .await
        .unwrap();
    storage
        .log_observed(
            observed(&whatsapp, "bob", "b1", 150, Some("hey")),
            Some("Bob".into()),
        )
        .await
        .unwrap();
    // A later message without a title must not clear the stored title, and must
    // advance the chat's preview/timestamp.
    storage
        .log_observed(
            observed(&whatsapp, "alice", "a2", 200, Some("second")),
            None,
        )
        .await
        .unwrap();

    let chats = storage.list_observed_chats().await.unwrap();
    assert_eq!(chats.len(), 2);
    assert_eq!(chats[0].chat_id, "alice"); // most recent (t=200) first
    assert_eq!(chats[0].title.as_deref(), Some("Alice")); // retained
    assert_eq!(chats[0].last_message_ts, 200);
    assert_eq!(chats[0].last_message_preview, "second");
    assert_eq!(chats[1].chat_id, "bob");

    let page = storage
        .observed_messages(&whatsapp, "alice", None, 10)
        .await
        .unwrap();
    assert_eq!(
        page.iter()
            .map(|m| m.native_id.as_str())
            .collect::<Vec<_>>(),
        ["a2", "a1"], // newest first
    );

    let older = storage
        .observed_messages(&whatsapp, "alice", Some(200), 10)
        .await
        .unwrap();
    assert_eq!(older.len(), 1);
    assert_eq!(older[0].native_id, "a1");

    let edited = storage
        .mark_observed_edited(&whatsapp, "alice", "a1", Some("first (edited)".into()))
        .await
        .unwrap()
        .expect("message exists");
    assert!(edited.edited);
    assert_eq!(edited.text.as_deref(), Some("first (edited)"));

    let deleted = storage
        .mark_observed_deleted(&whatsapp, "alice", "a2")
        .await
        .unwrap()
        .expect("message exists");
    assert!(deleted.deleted);

    // Markers on an unknown message report nothing.
    assert!(
        storage
            .mark_observed_deleted(&whatsapp, "alice", "missing")
            .await
            .unwrap()
            .is_none()
    );

    // Mutations persist.
    let page = storage
        .observed_messages(&whatsapp, "alice", None, 10)
        .await
        .unwrap();
    let a1 = page.iter().find(|m| m.native_id == "a1").unwrap();
    assert!(a1.edited && a1.text.as_deref() == Some("first (edited)"));
    let a2 = page.iter().find(|m| m.native_id == "a2").unwrap();
    assert!(a2.deleted);
}

fn observed(
    connector_id: &ConnectorId,
    chat_id: &str,
    platform_message_id: &str,
    timestamp_ms: i64,
    text: Option<&str>,
) -> ObservedMessage {
    ObservedMessage {
        connector_id: connector_id.clone(),
        platform: Platform::WhatsApp,
        chat_id: chat_id.into(),
        thread_id: None,
        native_id: platform_message_id.into(),
        author_id: "sender".into(),
        author_name: None,
        participant: None,
        text: text.map(Into::into),
        attachments: vec![],
        reply_to_native_id: None,
        direction: ObservedDirection::Incoming,
        timestamp_ms,
        deleted: false,
        edited: false,
    }
}

fn command(message_id: MessageId, endpoint_id: EndpointId) -> DispatchCommand {
    DispatchCommand {
        target_endpoint: endpoint_id,
        target_connector: ConnectorId("telegram".into()),
        target: wuphf_model::TargetAddress {
            address: wuphf_model::ChatAddress::new("-100", Some("7".into())),
            create_thread: None,
        },
        payload: Action::Send {
            message: message(message_id),
            reply: None,
        },
    }
}

fn edit_command(message_id: MessageId, endpoint_id: EndpointId) -> DispatchCommand {
    DispatchCommand {
        target_endpoint: endpoint_id,
        target_connector: ConnectorId("telegram".into()),
        target: wuphf_model::TargetAddress {
            address: wuphf_model::ChatAddress::new("-100", Some("7".into())),
            create_thread: None,
        },
        payload: Action::Edit {
            message: message(message_id),
            native_id: String::new(),
        },
    }
}

#[tokio::test]
async fn memory_outgoing_direction_survives_the_platform_echo() {
    outgoing_direction_survives_the_platform_echo(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_outgoing_direction_survives_the_platform_echo() {
    let path = std::env::temp_dir().join(format!("wuphf-outgoing-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    outgoing_direction_survives_the_platform_echo(SqliteStorage::connect(&url).await.unwrap())
        .await;
}

/// A row logged as outgoing (an owner message) must stay outgoing when the platform
/// reports the same message back as an inbound event: both backends return and
/// store the merged row with the original direction.
async fn outgoing_direction_survives_the_platform_echo<S: Storage>(storage: S) {
    let whatsapp = ConnectorId("whatsapp".into());
    let mut sent = observed(&whatsapp, "alice", "raw-1", 100, Some("mine"));
    sent.direction = ObservedDirection::Outgoing;
    let stored = storage.log_observed(sent, None).await.unwrap();
    assert_eq!(stored.direction, ObservedDirection::Outgoing);

    // The echo arrives as an ordinary incoming create for the same native id.
    let echo = observed(&whatsapp, "alice", "raw-1", 100, Some("mine"));
    let stored = storage.log_observed(echo, None).await.unwrap();
    assert_eq!(stored.direction, ObservedDirection::Outgoing);

    let page = storage
        .observed_messages(&whatsapp, "alice", None, 10)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].direction, ObservedDirection::Outgoing);
}

#[tokio::test]
async fn memory_provisioning_stash_drains_in_insertion_order() {
    provisioning_stash_drains_in_insertion_order(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_provisioning_stash_drains_in_insertion_order() {
    let path = std::env::temp_dir().join(format!("wuphf-stash-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    provisioning_stash_drains_in_insertion_order(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// Stashed messages drain in insertion order resolved back to full messages;
/// stashing the same message twice is a no-op; a second drain is empty; and
/// `stashed_replica_sets` lists the set before the drain and not after.
async fn provisioning_stash_drains_in_insertion_order<S: Storage>(storage: S) {
    let replica_set_id = ReplicaSetId::new();
    let endpoint_id = EndpointId::new();
    let first = MessageId::new();
    let second = MessageId::new();
    storage.put_message(message(first)).await.unwrap();
    storage.put_message(message(second)).await.unwrap();

    storage
        .stash_provisioning_message(replica_set_id, first, endpoint_id, "Title")
        .await
        .unwrap();
    // A duplicate stash of the same message must not add a second row.
    storage
        .stash_provisioning_message(replica_set_id, first, endpoint_id, "Title")
        .await
        .unwrap();
    storage
        .stash_provisioning_message(replica_set_id, second, endpoint_id, "Title")
        .await
        .unwrap();

    assert_eq!(
        storage.stashed_replica_sets().await.unwrap(),
        vec![replica_set_id]
    );

    let drained = storage
        .drain_provisioning_stash(replica_set_id)
        .await
        .unwrap();
    assert_eq!(
        drained.iter().map(|(m, _, _)| m.id).collect::<Vec<_>>(),
        vec![first, second],
    );
    assert!(
        drained
            .iter()
            .all(|(_, origin, title)| *origin == endpoint_id && title == "Title")
    );

    // The drain removed the rows: a second drain is empty and the set is gone.
    assert!(
        storage
            .drain_provisioning_stash(replica_set_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(storage.stashed_replica_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn memory_address_links_resolve_from_either_side() {
    address_links_resolve_from_either_side(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_address_links_resolve_from_either_side() {
    let path = std::env::temp_dir().join(format!("wuphf-links-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    address_links_resolve_from_either_side(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// A link is symmetric and idempotent, and is scoped to its connector: the same
/// chat id on another connector is a different chat. Both backends must agree,
/// since rule matching reads this to decide whether a chat is listed.
async fn address_links_resolve_from_either_side<S: Storage>(storage: S) {
    let connector = ConnectorId("whatsapp-main".into());
    let phone = ChatAddress::new("15550001111@s.whatsapp.net", None);
    let lid = ChatAddress::new("88887777@lid", None);
    let link = AddressLink {
        connector_id: connector.clone(),
        address: phone.clone(),
        linked_address: lid.clone(),
    };
    storage.put_address_link(link.clone()).await.unwrap();
    // Re-reporting the same link is a no-op rather than a duplicate row.
    storage.put_address_link(link).await.unwrap();

    assert_eq!(
        storage.linked_addresses(&connector, &phone).await.unwrap(),
        vec![lid.clone()]
    );
    assert_eq!(
        storage.linked_addresses(&connector, &lid).await.unwrap(),
        vec![phone.clone()]
    );
    assert!(
        storage
            .linked_addresses(&ConnectorId("telegram-main".into()), &phone)
            .await
            .unwrap()
            .is_empty()
    );
    // A chat with no reported counterpart has no forms but its own.
    assert!(
        storage
            .linked_addresses(&connector, &ChatAddress::new("111@g.us", None))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn memory_rename_observed_chat_merges_both_forms() {
    rename_observed_chat_merges_both_forms(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_rename_observed_chat_merges_both_forms() {
    let path = std::env::temp_dir().join(format!("wuphf-rename-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    rename_observed_chat_merges_both_forms(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// Rows logged under a chat's old address move onto the new one, so a chat the
/// platform later reveals to be one conversation stops showing as two — and an
/// edit or delete arriving under the new address finds the moved row.
async fn rename_observed_chat_merges_both_forms<S: Storage>(storage: S) {
    let connector = ConnectorId("whatsapp-main".into());
    let phone = "15550001111@s.whatsapp.net";
    let lid = "88887777@lid";

    storage
        .log_observed(
            observed(&connector, phone, "m-1", 10, Some("hi")),
            Some("Alice".into()),
        )
        .await
        .unwrap();
    storage
        .log_observed(observed(&connector, lid, "m-2", 20, Some("hi")), None)
        .await
        .unwrap();
    assert_eq!(storage.list_observed_chats().await.unwrap().len(), 2);

    storage
        .rename_observed_chat(&connector, phone, lid)
        .await
        .unwrap();

    let chats = storage.list_observed_chats().await.unwrap();
    assert_eq!(
        chats.len(),
        1,
        "the two forms should collapse into one chat"
    );
    assert_eq!(chats[0].chat_id, lid);
    // The newer message keeps the summary, and the title known only to the old
    // form survives the merge.
    assert_eq!(chats[0].last_message_ts, 20);
    assert_eq!(chats[0].title.as_deref(), Some("Alice"));

    let messages = storage
        .observed_messages(&connector, lid, None, 10)
        .await
        .unwrap();
    assert_eq!(
        messages.len(),
        2,
        "both messages should live under one chat"
    );
    assert!(messages.iter().all(|message| message.chat_id == lid));
    assert!(
        storage
            .observed_messages(&connector, phone, None, 10)
            .await
            .unwrap()
            .is_empty()
    );

    // A delete reported under the new address now finds the row that was logged
    // under the old one.
    let deleted = storage
        .mark_observed_deleted(&connector, lid, "m-1")
        .await
        .unwrap()
        .expect("the moved row is reachable under the new address");
    assert!(deleted.deleted);
}
