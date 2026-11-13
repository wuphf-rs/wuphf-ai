use super::*;
use wuphf_model::{
    Action, Attachment, AttachmentKind, DedupKey, DispatchState, MediaBlob, MediaId, MessageCopy,
    ObservedDirection, ObservedMessage, Platform, ReceiptKind, Target,
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
    let parent = enqueue(&storage, send(parent_id, endpoint_id, None)).await;
    enqueue(&storage, send(reply_id, endpoint_id, Some(parent_id))).await;
    let released = storage
        .acknowledge_dispatch(parent.id, "telegram-parent".into())
        .await
        .unwrap();
    assert_eq!(released.len(), 1);
    let Action::Send {
        reply: reply_context,
        ..
    } = &released[0].action
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
            conversation_id: ConversationId::new(),
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
    let create = enqueue(&storage, send(message_id, endpoint_id, None)).await;
    enqueue(&storage, edit(message_id, endpoint_id, Some(message_id))).await;

    let released = storage
        .acknowledge_dispatch(create.id, "native-1".into())
        .await
        .unwrap();
    assert_eq!(released.len(), 1);
    let Action::Edit { native_id, .. } = &released[0].action else {
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
    let conversation_id = message(message_id).conversation_id;
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
    let blocked = enqueue(
        &storage,
        send(MessageId::new(), endpoint_id, Some(message_id)),
    )
    .await;

    let signals = [
        Action::React {
            message_id,
            native_id: "origin-1".into(),
            reactor: "someone".into(),
            reactor_name: None,
            emoji: Some("👍".into()),
        },
        Action::Receipt {
            message_id,
            native_id: "origin-1".into(),
            kind: ReceiptKind::Delivered,
        },
        Action::Receipt {
            message_id,
            native_id: "origin-1".into(),
            kind: ReceiptKind::Read,
        },
    ];
    for action in signals {
        let signal = enqueue(
            &storage,
            Dispatch::new(target(endpoint_id), conversation_id, action, None),
        )
        .await;
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
    let dispatch = enqueue(&storage, send(message_id, endpoint_id, None)).await;
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
async fn memory_conversation_endpoints_are_indexed_and_added_idempotently() {
    conversation_endpoints_are_indexed_and_added_idempotently(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_conversation_endpoints_are_indexed_and_added_idempotently() {
    let path = std::env::temp_dir().join(format!("wuphf-set-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    conversation_endpoints_are_indexed_and_added_idempotently(
        SqliteStorage::connect(&url).await.unwrap(),
    )
    .await;
}

/// `conversation_for_endpoint` finds the conversation listing an endpoint,
/// whether it was listed at creation or added later, and adding the same
/// endpoint twice leaves one entry.
async fn conversation_endpoints_are_indexed_and_added_idempotently<S: Storage>(storage: S) {
    let endpoint_a = EndpointId::new();
    let endpoint_b = EndpointId::new();
    let conversation = conversation(vec![endpoint_a]);
    storage
        .put_conversation(conversation.clone())
        .await
        .unwrap();

    storage
        .add_conversation_endpoint(conversation.id, endpoint_b)
        .await
        .unwrap();
    storage
        .add_conversation_endpoint(conversation.id, endpoint_b)
        .await
        .unwrap();

    for endpoint_id in [endpoint_a, endpoint_b] {
        let found = storage
            .conversation_for_endpoint(endpoint_id)
            .await
            .unwrap()
            .expect("the endpoint belongs to the conversation");
        assert_eq!(found.id, conversation.id);
        assert_eq!(found.endpoints, vec![endpoint_a, endpoint_b]);
    }
    assert_eq!(
        storage
            .conversation_for_endpoint(EndpointId::new())
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn memory_setting_a_title_keeps_the_endpoints() {
    setting_a_title_keeps_the_endpoints(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_setting_a_title_keeps_the_endpoints() {
    let path = std::env::temp_dir().join(format!("wuphf-title-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    setting_a_title_keeps_the_endpoints(SqliteStorage::connect(&url).await.unwrap()).await;
}

/// A title update writes only the title, so an endpoint added since the
/// caller read the conversation survives it. Both narrow writers reject an
/// unknown conversation.
async fn setting_a_title_keeps_the_endpoints<S: Storage>(storage: S) {
    let endpoint_a = EndpointId::new();
    let endpoint_b = EndpointId::new();
    let conversation = conversation(vec![endpoint_a]);
    storage
        .put_conversation(conversation.clone())
        .await
        .unwrap();
    storage
        .add_conversation_endpoint(conversation.id, endpoint_b)
        .await
        .unwrap();

    storage
        .set_conversation_title(conversation.id, "Priya")
        .await
        .unwrap();

    let stored = storage
        .conversation(conversation.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.title.as_deref(), Some("Priya"));
    assert_eq!(stored.endpoints, vec![endpoint_a, endpoint_b]);

    let missing = ConversationId::new();
    assert!(matches!(
        storage.set_conversation_title(missing, "x").await,
        Err(StorageError::ConversationNotFound(id)) if id == missing
    ));
    assert!(matches!(
        storage.add_conversation_endpoint(missing, endpoint_a).await,
        Err(StorageError::ConversationNotFound(id)) if id == missing
    ));
}

fn conversation(endpoints: Vec<EndpointId>) -> Conversation {
    Conversation {
        id: ConversationId::new(),
        rule_id: Some(wuphf_model::RuleId("mirror".into())),
        source: wuphf_model::ChatRef {
            connector_id: ConnectorId("whatsapp".into()),
            address: wuphf_model::ChatAddress::new("wa-chat", None),
        },
        endpoints,
        title: None,
        introduced: false,
    }
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
        conversation_id: ConversationId::new(),
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
    native_id: &str,
    timestamp_ms: i64,
    text: Option<&str>,
) -> ObservedMessage {
    ObservedMessage {
        connector_id: connector_id.clone(),
        platform: Platform::WhatsApp,
        chat_id: chat_id.into(),
        thread_id: None,
        native_id: native_id.into(),
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

/// Save a dispatch and hand it back, for tests that go on to settle it.
async fn enqueue<S: Storage>(storage: &S, dispatch: Dispatch) -> Dispatch {
    storage.enqueue_dispatch(&dispatch).await.unwrap();
    dispatch
}

fn target(endpoint_id: EndpointId) -> Target {
    Target {
        endpoint_id,
        connector_id: ConnectorId("telegram".into()),
        address: wuphf_model::ChatAddress::new("-100", Some("7".into())),
        create_thread: None,
    }
}

fn send(
    message_id: MessageId,
    endpoint_id: EndpointId,
    waiting_for: Option<MessageId>,
) -> Dispatch {
    let message = message(message_id);
    Dispatch::new(
        target(endpoint_id),
        message.conversation_id,
        Action::Send {
            message,
            reply: None,
            disappearing: None,
        },
        waiting_for,
    )
}

fn edit(
    message_id: MessageId,
    endpoint_id: EndpointId,
    waiting_for: Option<MessageId>,
) -> Dispatch {
    let message = message(message_id);
    Dispatch::new(
        target(endpoint_id),
        message.conversation_id,
        Action::Edit {
            message,
            native_id: String::new(),
        },
        waiting_for,
    )
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
/// `stashed_conversations` lists the conversation before the drain and not after.
async fn provisioning_stash_drains_in_insertion_order<S: Storage>(storage: S) {
    let conversation_id = ConversationId::new();
    let endpoint_id = EndpointId::new();
    let first = MessageId::new();
    let second = MessageId::new();
    storage.put_message(message(first)).await.unwrap();
    storage.put_message(message(second)).await.unwrap();

    storage
        .stash_provisioning_message(conversation_id, first, endpoint_id, "Title")
        .await
        .unwrap();
    // A duplicate stash of the same message must not add a second row.
    storage
        .stash_provisioning_message(conversation_id, first, endpoint_id, "Title")
        .await
        .unwrap();
    storage
        .stash_provisioning_message(conversation_id, second, endpoint_id, "Title")
        .await
        .unwrap();

    assert_eq!(
        storage.stashed_conversations().await.unwrap(),
        vec![conversation_id]
    );

    let drained = storage
        .drain_provisioning_stash(conversation_id)
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
            .drain_provisioning_stash(conversation_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(storage.stashed_conversations().await.unwrap().is_empty());
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

fn setting(key: &str, value: &str, updated_by: Option<&str>) -> StoredSetting {
    StoredSetting {
        key: key.into(),
        value: value.into(),
        updated_by: updated_by.map(Into::into),
    }
}

#[tokio::test]
async fn memory_settings_overrides_round_trip() {
    settings_overrides_round_trip(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_settings_overrides_round_trip() {
    let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
    let storage = SqliteStorage::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    settings_overrides_round_trip(storage).await;
}

/// Upserts replace by key and load in key order; one write deletes some keys
/// and upserts another together.
async fn settings_overrides_round_trip<S: SettingsOverrides>(storage: S) {
    let b = setting("b", "1", None);
    let a = setting("a", "\"x\"", Some("telegram:user:7"));
    let b2 = setting("b", "2", Some("telegram:user:8"));
    for upserted in [&b, &a, &b2] {
        storage
            .write_settings_overrides(&[], Some(upserted), 1)
            .await
            .unwrap();
    }
    assert_eq!(
        storage.settings_overrides().await.unwrap(),
        [a.clone(), b2.clone()]
    );

    storage
        .write_settings_overrides(&["a".into(), "missing".into()], None, 2)
        .await
        .unwrap();
    assert_eq!(storage.settings_overrides().await.unwrap(), [b2]);

    let c = setting("c", "[]", None);
    storage
        .write_settings_overrides(&["b".into()], Some(&c), 3)
        .await
        .unwrap();
    assert_eq!(storage.settings_overrides().await.unwrap(), [c]);
}

#[tokio::test]
async fn sqlite_settings_overrides_survive_reopen() {
    let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    let stored = setting("rules", "{}", Some("whatsapp:1@lid"));
    SqliteStorage::connect(&url)
        .await
        .unwrap()
        .write_settings_overrides(&[], Some(&stored), 1)
        .await
        .unwrap();

    let reopened = SqliteStorage::connect(&url).await.unwrap();
    assert_eq!(reopened.settings_overrides().await.unwrap(), [stored]);
}

/// A fresh SQLite database in the temp directory.
async fn temp_sqlite() -> SqliteStorage {
    let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
    SqliteStorage::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap()
}

#[tokio::test]
async fn memory_observed_chat_returns_the_latest_title() {
    observed_chat_returns_the_latest_title(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_observed_chat_returns_the_latest_title() {
    observed_chat_returns_the_latest_title(temp_sqlite().await).await;
}

/// One chat's summary: a newer title replaces the old one, a message without a
/// title keeps it, and an unseen chat has none.
async fn observed_chat_returns_the_latest_title<S: Storage>(storage: S) {
    let whatsapp = ConnectorId("whatsapp".into());
    for (id, ts, title) in [
        ("m1", 100, Some("Old")),
        ("m2", 200, Some("New")),
        ("m3", 300, None),
    ] {
        storage
            .log_observed(
                observed(&whatsapp, "family@g.us", id, ts, Some("hi")),
                title.map(Into::into),
            )
            .await
            .unwrap();
    }
    let chat = storage
        .observed_chat(&whatsapp, "family@g.us")
        .await
        .unwrap()
        .expect("the chat was observed");
    assert_eq!(chat.title.as_deref(), Some("New"));
    assert_eq!(chat.last_message_ts, 300);
    assert!(
        storage
            .observed_chat(&whatsapp, "unseen@g.us")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn memory_panel_tokens_round_trip() {
    panel_tokens_round_trip(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_panel_tokens_round_trip() {
    panel_tokens_round_trip(temp_sqlite().await).await;
}

async fn panel_tokens_round_trip<S: PanelTokens>(storage: S) {
    assert!(storage.put_panel_token("~k", "a", 0, 100).await.unwrap());
    assert_eq!(
        storage.panel_token("~k", 50).await.unwrap().as_deref(),
        Some("a")
    );
    assert!(storage.panel_token("~missing", 50).await.unwrap().is_none());
}

#[tokio::test]
async fn memory_putting_the_same_action_renews_it() {
    putting_the_same_action_renews_it(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_putting_the_same_action_renews_it() {
    putting_the_same_action_renews_it(temp_sqlite().await).await;
}

async fn putting_the_same_action_renews_it<S: PanelTokens>(storage: S) {
    assert!(storage.put_panel_token("~k", "a", 0, 100).await.unwrap());
    assert!(storage.put_panel_token("~k", "a", 50, 200).await.unwrap());
    assert_eq!(
        storage.panel_token("~k", 199).await.unwrap().as_deref(),
        Some("a"),
        "the second put renewed the expiry"
    );
}

#[tokio::test]
async fn memory_a_live_collision_is_refused() {
    a_live_collision_is_refused(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_a_live_collision_is_refused() {
    a_live_collision_is_refused(temp_sqlite().await).await;
}

/// A key holding another unexpired action is kept; once that action expired,
/// the key is reused.
async fn a_live_collision_is_refused<S: PanelTokens>(storage: S) {
    assert!(storage.put_panel_token("~k", "a", 0, 100).await.unwrap());
    assert!(!storage.put_panel_token("~k", "b", 50, 200).await.unwrap());
    assert_eq!(
        storage.panel_token("~k", 60).await.unwrap().as_deref(),
        Some("a")
    );
    assert!(storage.put_panel_token("~k", "b", 101, 300).await.unwrap());
    assert_eq!(
        storage.panel_token("~k", 150).await.unwrap().as_deref(),
        Some("b")
    );
}

#[tokio::test]
async fn memory_expired_panel_tokens_are_not_returned() {
    expired_panel_tokens_are_not_returned(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_expired_panel_tokens_are_not_returned() {
    expired_panel_tokens_are_not_returned(temp_sqlite().await).await;
}

async fn expired_panel_tokens_are_not_returned<S: PanelTokens>(storage: S) {
    storage.put_panel_token("~k", "a", 0, 100).await.unwrap();
    assert!(storage.panel_token("~k", 100).await.unwrap().is_some());
    assert!(storage.panel_token("~k", 101).await.unwrap().is_none());
}

#[tokio::test]
async fn memory_purge_removes_only_expired_tokens() {
    purge_removes_only_expired_tokens(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_purge_removes_only_expired_tokens() {
    purge_removes_only_expired_tokens(temp_sqlite().await).await;
}

async fn purge_removes_only_expired_tokens<S: PanelTokens>(storage: S) {
    storage.put_panel_token("~old", "a", 0, 100).await.unwrap();
    storage.put_panel_token("~older", "b", 0, 50).await.unwrap();
    storage.put_panel_token("~live", "c", 0, 500).await.unwrap();
    assert_eq!(storage.purge_panel_tokens(200).await.unwrap(), 2);
    assert_eq!(
        storage.panel_token("~live", 200).await.unwrap().as_deref(),
        Some("c")
    );
    assert_eq!(storage.purge_panel_tokens(200).await.unwrap(), 0);
}

fn declaration(connector: &str, base_json: &str) -> SettingsDeclaration {
    SettingsDeclaration {
        connector_id: ConnectorId(connector.into()),
        descriptor: vec![0x0a, 0x00, 0xff],
        json_schema: r#"{"type": "object"}"#.into(),
        base_json: base_json.into(),
    }
}

#[tokio::test]
async fn memory_settings_declarations_replace_by_connector() {
    settings_declarations_replace_by_connector(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_settings_declarations_replace_by_connector() {
    settings_declarations_replace_by_connector(temp_sqlite().await).await;
}

/// A connector's new declaration replaces its old one; declarations load in
/// connector order, the descriptor bytes intact.
async fn settings_declarations_replace_by_connector<S: SettingsDeclarations>(storage: S) {
    let whatsapp = declaration("whatsapp-main", r#"{"log_level": "info"}"#);
    let telegram = declaration("telegram-main", r#"{"log_level": "info"}"#);
    let telegram2 = declaration("telegram-main", r#"{"log_level": "debug"}"#);
    for (stored, at) in [(&whatsapp, 1), (&telegram, 2), (&telegram2, 3)] {
        storage.put_settings_declaration(stored, at).await.unwrap();
    }
    assert_eq!(
        storage.settings_declarations().await.unwrap(),
        [telegram2, whatsapp]
    );
}

fn chat_profile(connector: &str, chat_id: &str, seconds: Option<u32>) -> ChatProfile {
    ChatProfile {
        disappearing: seconds.map(|seconds| wuphf_model::DisappearingTimer {
            seconds,
            setting_timestamp: Some(1_700_000_000),
        }),
        ..ChatProfile::new(ConnectorId(connector.into()), chat_id)
    }
}

#[tokio::test]
async fn memory_chat_profiles_round_trip() {
    chat_profiles_round_trip(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_chat_profiles_round_trip() {
    chat_profiles_round_trip(temp_sqlite().await).await;
}

/// Profiles are keyed by connector and chat: a put replaces, the same chat id
/// on another connector is a separate profile, and an unknown timer survives.
async fn chat_profiles_round_trip<S: ChatProfileStore>(storage: S) {
    let whatsapp = ConnectorId("whatsapp".into());
    assert_eq!(
        storage
            .chat_profile(&whatsapp, "1@s.whatsapp.net")
            .await
            .unwrap(),
        None
    );

    let day = chat_profile("whatsapp", "1@s.whatsapp.net", Some(86_400));
    storage.put_chat_profile(&day).await.unwrap();
    assert_eq!(
        storage
            .chat_profile(&whatsapp, "1@s.whatsapp.net")
            .await
            .unwrap(),
        Some(day)
    );

    let week = chat_profile("whatsapp", "1@s.whatsapp.net", Some(604_800));
    storage.put_chat_profile(&week).await.unwrap();
    assert_eq!(
        storage
            .chat_profile(&whatsapp, "1@s.whatsapp.net")
            .await
            .unwrap(),
        Some(week.clone())
    );

    let other = chat_profile("whatsapp-2", "1@s.whatsapp.net", Some(0));
    storage.put_chat_profile(&other).await.unwrap();
    assert_eq!(
        storage
            .chat_profile(&whatsapp, "1@s.whatsapp.net")
            .await
            .unwrap(),
        Some(week)
    );
    assert_eq!(
        storage
            .chat_profile(&ConnectorId("whatsapp-2".into()), "1@s.whatsapp.net")
            .await
            .unwrap(),
        Some(other)
    );

    let unknown = chat_profile("whatsapp", "2@s.whatsapp.net", None);
    storage.put_chat_profile(&unknown).await.unwrap();
    assert_eq!(
        storage
            .chat_profile(&whatsapp, "2@s.whatsapp.net")
            .await
            .unwrap(),
        Some(unknown)
    );
}

#[tokio::test]
async fn sqlite_chat_profiles_survive_reopen() {
    let path = std::env::temp_dir().join(format!("wuphf-{}.sqlite", DispatchId::new()));
    let url = format!("sqlite://{}", path.display());
    let stored = chat_profile("whatsapp", "1@g.us", Some(604_800));
    SqliteStorage::connect(&url)
        .await
        .unwrap()
        .put_chat_profile(&stored)
        .await
        .unwrap();

    let reopened = SqliteStorage::connect(&url).await.unwrap();
    assert_eq!(
        reopened
            .chat_profile(&ConnectorId("whatsapp".into()), "1@g.us")
            .await
            .unwrap(),
        Some(stored)
    );
}

#[tokio::test]
async fn memory_chat_profiles_round_trip_every_field() {
    chat_profiles_round_trip_every_field(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_chat_profiles_round_trip_every_field() {
    chat_profiles_round_trip_every_field(temp_sqlite().await).await;
}

/// Every profile field survives a round trip, and a later put with fewer
/// fields replaces the row rather than merging into it.
async fn chat_profiles_round_trip_every_field<S: ChatProfileStore>(storage: S) {
    let whatsapp = ConnectorId("whatsapp".into());
    let full = ChatProfile {
        title: Some("[G] Family".into()),
        description: Some("Weekend plans".into()),
        avatar: Some(wuphf_model::ChatAvatar {
            id: "pic-1".into(),
            media_id: Some(MediaId::new()),
            hash: Some("ab".repeat(32)),
        }),
        participant_count: Some(12),
        pinned: Some(true),
        mute: Some(wuphf_model::MuteState {
            muted_until: wuphf_model::MuteState::FOREVER,
        }),
        archived: Some(false),
        ..chat_profile("whatsapp", "1@g.us", Some(604_800))
    };
    storage.put_chat_profile(&full).await.unwrap();
    assert_eq!(
        storage.chat_profile(&whatsapp, "1@g.us").await.unwrap(),
        Some(full)
    );

    let fewer = ChatProfile {
        title: Some("[G] Family".into()),
        ..ChatProfile::new(whatsapp.clone(), "1@g.us")
    };
    storage.put_chat_profile(&fewer).await.unwrap();
    assert_eq!(
        storage.chat_profile(&whatsapp, "1@g.us").await.unwrap(),
        Some(fewer)
    );
}

#[tokio::test]
async fn memory_marking_a_conversation_introduced_keeps_the_rest() {
    marking_a_conversation_introduced_keeps_the_rest(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_marking_a_conversation_introduced_keeps_the_rest() {
    marking_a_conversation_introduced_keeps_the_rest(temp_sqlite().await).await;
}

/// The flag is a narrow write: the title and an endpoint added since the
/// caller read the conversation both survive it.
async fn marking_a_conversation_introduced_keeps_the_rest<S: Storage>(storage: S) {
    let endpoint_a = EndpointId::new();
    let endpoint_b = EndpointId::new();
    let conversation = conversation(vec![endpoint_a]);
    storage
        .put_conversation(conversation.clone())
        .await
        .unwrap();
    storage
        .set_conversation_title(conversation.id, "Priya")
        .await
        .unwrap();
    storage
        .add_conversation_endpoint(conversation.id, endpoint_b)
        .await
        .unwrap();

    storage
        .mark_conversation_introduced(conversation.id)
        .await
        .unwrap();

    let stored = storage
        .conversation(conversation.id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.introduced);
    assert_eq!(stored.title.as_deref(), Some("Priya"));
    assert_eq!(stored.endpoints, vec![endpoint_a, endpoint_b]);

    let missing = ConversationId::new();
    assert!(matches!(
        storage.mark_conversation_introduced(missing).await,
        Err(StorageError::ConversationNotFound(id)) if id == missing
    ));
}

#[tokio::test]
async fn memory_pruning_spares_media_a_profile_references() {
    pruning_spares_media_a_profile_references(InMemoryStorage::default()).await;
}

#[tokio::test]
async fn sqlite_pruning_spares_media_a_profile_references() {
    pruning_spares_media_a_profile_references(temp_sqlite().await).await;
}

/// A chat's stored avatar is no orphan: pruning keeps it while dropping an
/// equally old blob nothing references.
async fn pruning_spares_media_a_profile_references<S: Storage>(storage: S) {
    let avatar = attachment();
    let orphan = attachment();
    for attachment in [&avatar, &orphan] {
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
        .put_chat_profile(&ChatProfile {
            avatar: Some(wuphf_model::ChatAvatar {
                id: "pic-1".into(),
                media_id: Some(avatar.media_id),
                hash: None,
            }),
            ..ChatProfile::new(ConnectorId("whatsapp".into()), "1@g.us")
        })
        .await
        .unwrap();

    assert_eq!(
        storage.prune_unreferenced_media_before(20).await.unwrap(),
        1
    );
    assert!(storage.media(avatar.media_id).await.unwrap().is_some());
    assert!(storage.media(orphan.media_id).await.unwrap().is_none());
}
