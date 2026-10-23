use super::*;
use wuphf_model::{
    Action, Attachment, AttachmentKind, ChatAddress, ChatFilter, Connector, ConnectorCapabilities,
    Dispatch, DispatchAck, MediaBlob, MediaId, NewMessage, NoticeKind, OWNER_AUTHOR, OwnerMessage,
    Platform, ReceiptKind, Rule, RuleId, RuleKind, SourceAddress,
};
use wuphf_storage::{InMemoryStorage, MediaStore, ObservationStore, TopologyStore};

const WHATSAPP: &str = "whatsapp-main";
const TELEGRAM: &str = "telegram-main";

fn connectors() -> Vec<Connector> {
    vec![
        Connector {
            id: ConnectorId(WHATSAPP.into()),
            platform: Platform::WhatsApp,
            capabilities: ConnectorCapabilities::default(),
        },
        Connector {
            id: ConnectorId(TELEGRAM.into()),
            platform: Platform::Telegram,
            capabilities: ConnectorCapabilities {
                supports_threads: true,
                supports_topic_create: true,
            },
        },
    ]
}

fn direct_rule() -> Rule {
    Rule {
        id: RuleId("direct".into()),
        flow: RuleKind::OneToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chat: ChatAddress {
                chat_id: "12345@s.whatsapp.net".into(),
                thread_id: None,
            },
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress {
                chat_id: "-100123".into(),
                thread_id: Some("12".into()),
            },
        },
    }
}

fn mirror_rule() -> Rule {
    Rule {
        id: RuleId("wa-mirror".into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chats_filter: ChatFilter::Only {
                chats: vec![ChatAddress {
                    chat_id: "111@g.us".into(),
                    thread_id: None,
                }],
            },
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress {
                chat_id: "-1001234567".into(),
                thread_id: None,
            },
        },
    }
}

fn core(rules: Vec<Rule>) -> Core<InMemoryStorage> {
    core_over(Arc::new(InMemoryStorage::default()), rules)
}

/// Build a core over caller-owned storage, so a test can construct a second core
/// over the same storage to simulate a process restart (the in-memory
/// provisioning cache starts empty on the new core).
fn core_over(storage: Arc<InMemoryStorage>, rules: Vec<Rule>) -> Core<InMemoryStorage> {
    let config = Arc::new(RoutingConfig::new(rules, connectors()));
    Core::new(storage, config)
}

fn event(connector: &str, chat_id: &str, native_id: &str, reply_to: Option<&str>) -> Event {
    event_in(connector, chat_id, None, native_id, reply_to)
}

fn event_in(
    connector: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    native_id: &str,
    reply_to: Option<&str>,
) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(connector.into()),
            address: ChatAddress::new(chat_id, thread_id.map(str::to_owned)),
            chat_title: Some("Title".into()),
        },
        payload: EventKind::NewMessage(NewMessage {
            native_id: native_id.into(),
            author_id: "alice".into(),
            participant: None,
            author_name: None,
            text: Some("hello".into()),
            attachments: vec![],
            reply_to_native_id: reply_to.map(str::to_owned),
            timestamp_ms: 1,
        }),
    }
}

#[tokio::test]
async fn direct_rule_mints_endpoints_and_fans_out_to_the_paired_single() {
    let core = core(vec![direct_rule()]);
    let outcome = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    let dispatches = outcome.dispatches();
    assert_eq!(dispatches.len(), 1);
    let to = &dispatches[0].to;
    assert_eq!(to.connector_id, ConnectorId(TELEGRAM.into()));
    assert_eq!(to.address, ChatAddress::new("-100123", Some("12".into())));
    assert!(to.create_thread.is_none());
}

#[tokio::test]
async fn unmatched_address_is_ignored() {
    let core = core(vec![direct_rule()]);
    let outcome = core
        .ingest(event(WHATSAPP, "99999@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
}

#[tokio::test]
async fn source_excluded_by_filter_is_ignored() {
    let core = core(vec![mirror_rule()]);
    let outcome = core
        .ingest(event(WHATSAPP, "222@g.us", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
}

#[tokio::test]
async fn mirror_dispatch_requests_thread_provisioning_then_persists_mapping() {
    let core = core(vec![mirror_rule()]);
    let outcome = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    let dispatch = &outcome.dispatches()[0];
    let title = dispatch
        .to
        .create_thread
        .as_ref()
        .expect("first mirror send provisions a thread");
    assert_eq!(dispatch.to.address.chat_id, "-1001234567");
    assert_eq!(title, "Title");

    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: dispatch.id,
        native_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();

    // A reply typed into the provisioned topic resolves back to the source.
    let reverse = core
        .ingest(event_in(
            TELEGRAM,
            "-1001234567",
            Some("777"),
            "tg-reply",
            None,
        ))
        .await
        .unwrap();
    let back = &reverse.dispatches()[0];
    assert_eq!(back.to.connector_id, ConnectorId(WHATSAPP.into()));
    assert_eq!(back.to.address.chat_id, "111@g.us");

    // The next source message reuses the thread without provisioning.
    let again = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
        .await
        .unwrap();
    let resend = &again.dispatches()[0];
    assert_eq!(
        resend.to.address,
        ChatAddress::new("-1001234567", Some("777".into()))
    );
    assert!(resend.to.create_thread.is_none());
}

#[tokio::test]
async fn source_rename_retitles_the_provisioned_mirror_thread() {
    let core = core(vec![mirror_rule()]);
    let first = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: first.dispatches()[0].id,
        native_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();

    let mut renamed = event(WHATSAPP, "111@g.us", "wa-2", None);
    renamed.source.chat_title = Some("Renamed".into());
    let outcome = core.ingest(renamed).await.unwrap();
    let (retitle, title) = outcome
        .dispatches()
        .iter()
        .find_map(|stored| match &stored.action {
            Action::RenameThread { title, .. } => Some((stored, title.clone())),
            _ => None,
        })
        .expect("a source rename fans out a retitle dispatch");
    assert_eq!(title, "Renamed");
    assert_eq!(retitle.to.connector_id, ConnectorId(TELEGRAM.into()));
    assert_eq!(
        retitle.to.address,
        ChatAddress::new("-1001234567", Some("777".into()))
    );

    // The same title on a later message fans out no further retitle.
    let mut unchanged = event(WHATSAPP, "111@g.us", "wa-3", None);
    unchanged.source.chat_title = Some("Renamed".into());
    let stable = core.ingest(unchanged).await.unwrap();
    assert!(
        stable
            .dispatches()
            .iter()
            .all(|stored| !matches!(stored.action, Action::RenameThread { .. }))
    );
}

#[tokio::test]
async fn first_title_retitles_a_thread_provisioned_without_one() {
    // A thread provisioned before any title was known is named with the raw
    // chat id; the first real title that arrives later must rename it.
    let core = core(vec![mirror_rule()]);
    let mut untitled = event(WHATSAPP, "111@g.us", "wa-1", None);
    untitled.source.chat_title = None;
    let first = core.ingest(untitled).await.unwrap();
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: first.dispatches()[0].id,
        native_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();

    let mut titled = event(WHATSAPP, "111@g.us", "wa-2", None);
    titled.source.chat_title = Some("Real Name".into());
    let outcome = core.ingest(titled).await.unwrap();
    let title = outcome
        .dispatches()
        .iter()
        .find_map(|stored| match &stored.action {
            Action::RenameThread { title, .. } => Some(title.clone()),
            _ => None,
        })
        .expect("the first real title retitles the existing thread");
    assert_eq!(title, "Real Name");
}

#[tokio::test]
async fn concurrent_messages_provision_one_thread_and_replay_the_rest() {
    let core = core(vec![mirror_rule()]);

    // The first source message provisions the mirror topic.
    let first = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    let IngestOutcome::Routed { dispatches, .. } = first else {
        panic!("first message routes");
    };
    assert_eq!(dispatches.len(), 1);
    assert!(dispatches[0].to.create_thread.is_some());

    // A second message arriving before the provision is acknowledged must be
    // deferred rather than provisioning a second topic for the same chat.
    let second = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
        .await
        .unwrap();
    let IngestOutcome::Deferred { .. } = second else {
        panic!("second message is deferred while provisioning");
    };

    // Acknowledging the provision replays the stashed message onto the now
    // known thread, without a second provision request.
    let replayed = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: dispatches[0].id,
            native_id: "tg-1".into(),
            thread_id: Some("777".into()),
        })
        .await
        .unwrap();
    // Acknowledging the create also emits a delivery-ack to the origin, so the
    // replayed stashed message is found among the released dispatches.
    let to = &replayed
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .expect("the stashed message is replayed")
        .to;
    assert_eq!(to.connector_id, ConnectorId(TELEGRAM.into()));
    assert!(to.create_thread.is_none());
    assert_eq!(to.address.thread_id, Some("777".into()));
}

#[tokio::test]
async fn restart_replays_stash_when_provisioning_settled_while_down() {
    let storage = Arc::new(InMemoryStorage::default());
    let core = core_over(Arc::clone(&storage), vec![mirror_rule()]);

    // A provisions the mirror thread; B arrives mid-provision and is deferred
    // into the durable stash.
    let first = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    let IngestOutcome::Routed { dispatches, .. } = first else {
        panic!("first message routes");
    };
    assert!(dispatches[0].to.create_thread.is_some());
    let provisioning_id = dispatches[0].id;
    let deferred = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
        .await
        .unwrap();
    assert!(matches!(deferred, IngestOutcome::Deferred { .. }));

    // Simulate a restart: a fresh core over the same storage begins with an empty
    // provisioning cache. recover_provisioning rebuilds the pending set from the
    // still-pending provision so the ack below can find and drain the durable
    // stash; nothing is orphaned yet, so it replays nothing now.
    drop(core);
    let core = core_over(storage, vec![mirror_rule()]);
    let recovered = core.recover_provisioning().await.unwrap();
    assert!(
        recovered.is_empty(),
        "the provision is still pending, so nothing is orphaned",
    );

    // Acknowledging the provision on the restarted core replays the durable stash
    // — the in-memory stash of the old core is gone, so this passes only because
    // the stash persisted and the pending set was rebuilt.
    let replayed = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: provisioning_id,
            native_id: "tg-1".into(),
            thread_id: Some("777".into()),
        })
        .await
        .unwrap();
    let replayed_b = replayed
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .expect("the stashed message replays after restart");
    assert!(replayed_b.to.create_thread.is_none());
    assert_eq!(replayed_b.to.address.thread_id, Some("777".into()));
}

#[tokio::test]
async fn failed_provisioning_releases_stashed_messages() {
    let core = core(vec![mirror_rule()]);

    // The first source message provisions the mirror topic.
    let first = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    let IngestOutcome::Routed { dispatches, .. } = first else {
        panic!("first message routes");
    };
    assert!(dispatches[0].to.create_thread.is_some());
    let provisioning_id = dispatches[0].id;

    // A second message is deferred (stashed) behind the in-flight provision.
    let second = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
        .await
        .unwrap();
    let IngestOutcome::Deferred { .. } = second else {
        panic!("second message is deferred while provisioning");
    };

    // Failing the provisioning dispatch must release the stash rather than strand
    // the conversation: the stashed message replays and re-attempts provisioning
    // (a fresh create still carrying a create_thread, since no thread exists).
    let released = core
        .fail_dispatch(provisioning_id, "cannot create topic".into())
        .await
        .unwrap();
    let replay = released
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .expect("the stashed message replays when provisioning fails");
    assert!(
        replay.to.create_thread.is_some(),
        "replay re-attempts provisioning",
    );

    // The failed provisioning dispatch is dead-lettered, not pending.
    assert!(
        core.pending_dispatches()
            .await
            .unwrap()
            .iter()
            .all(|dispatch| dispatch.id != provisioning_id),
        "a failed dispatch leaves the pending queue",
    );
}

#[tokio::test]
async fn ignores_echoes_of_dispatched_copies() {
    let core = core(vec![direct_rule()]);
    let outcome = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    let IngestOutcome::Routed {
        message_id,
        dispatches,
        ..
    } = outcome
    else {
        panic!("message should route");
    };
    let telegram = dispatches[0].to.endpoint_id;
    core.record_dispatched_copy(message_id, telegram, "tg-9")
        .await
        .unwrap();
    assert_eq!(
        core.ingest(event_in(TELEGRAM, "-100123", Some("12"), "tg-9", None))
            .await
            .unwrap(),
        IngestOutcome::Ignored(IgnoreReason::SentMessageEcho)
    );
}

#[tokio::test]
async fn ignores_duplicate_events() {
    let core = core(vec![direct_rule()]);
    let event = event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None);
    core.ingest(event.clone()).await.unwrap();
    assert_eq!(
        core.ingest(event).await.unwrap(),
        IngestOutcome::Ignored(IgnoreReason::DuplicateEvent)
    );
}

#[tokio::test]
async fn maps_replies_through_internal_identity_and_target_copy() {
    let core = core(vec![direct_rule()]);
    let parent = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-parent", None))
        .await
        .unwrap();
    let IngestOutcome::Routed {
        message_id: parent_id,
        dispatches,
        ..
    } = parent
    else {
        panic!("parent should route");
    };
    let telegram = dispatches[0].to.endpoint_id;
    core.record_dispatched_copy(parent_id, telegram, "tg-parent")
        .await
        .unwrap();

    let reply = core
        .ingest(event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-reply",
            Some("wa-parent"),
        ))
        .await
        .unwrap();
    let Action::Send {
        message,
        reply: reply_context,
        ..
    } = &reply.dispatches()[0].action
    else {
        panic!("reply should route as a message create");
    };
    assert_eq!(message.reply_to, Some(parent_id));
    let reply_context = reply_context.as_ref().expect("reply context resolved");
    assert_eq!(reply_context.message_id, parent_id);
    assert_eq!(reply_context.native_id, "tg-parent");
}

#[tokio::test]
async fn blocks_reply_dispatch_until_target_parent_copy_exists() {
    let core = core(vec![direct_rule()]);
    core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-parent", None))
        .await
        .unwrap();
    let reply = core
        .ingest(event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-reply",
            Some("wa-parent"),
        ))
        .await
        .unwrap();
    let IngestOutcome::Routed {
        dispatches,
        blocked,
        ..
    } = reply
    else {
        panic!("reply should be stored while delivery is blocked");
    };
    assert!(dispatches.is_empty());
    assert_eq!(blocked, 1);
}

#[tokio::test]
async fn migrated_address_resolves_to_the_same_endpoint() {
    let core = core(vec![mirror_rule()]);
    let first = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    let IngestOutcome::Routed {
        message_id,
        dispatches,
        ..
    } = first
    else {
        panic!("first message should route");
    };

    // Establish the mirror thread so later messages reuse it instead of
    // being held for provisioning.
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: dispatches[0].id,
        native_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();

    assert!(
        core.migrate_identity(
            ConnectorId(WHATSAPP.into()),
            ChatAddress::new("111@g.us", None),
            ChatAddress::new("111@lid", None),
        )
        .await
        .unwrap()
    );

    // The new address routes into the same conversation (no new mint), so a
    // message under it fans out to the already-provisioned mirror endpoint.
    let migrated = core
        .ingest(event(WHATSAPP, "111@lid", "wa-2", None))
        .await
        .unwrap();
    let IngestOutcome::Routed {
        message_id: migrated_id,
        dispatches,
        ..
    } = migrated
    else {
        panic!("migrated message should route");
    };
    assert_ne!(message_id, migrated_id);
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(TELEGRAM.into()));
}

#[tokio::test]
async fn rejects_unknown_or_modified_media_before_claiming_event() {
    let core = core(vec![direct_rule()]);
    let attachment = Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::Image,
        file_name: Some("photo.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        size_bytes: Some(5),
        animated: false,
    };
    let media_event = |attachment: Attachment| {
        let mut event = event(WHATSAPP, "12345@s.whatsapp.net", "wa-media", None);
        let EventKind::NewMessage(inbound) = &mut event.payload else {
            unreachable!();
        };
        inbound.attachments = vec![attachment];
        event
    };
    assert!(matches!(
        core.ingest(media_event(attachment.clone())).await,
        Err(CoreError::UnknownMedia(id)) if id == attachment.media_id
    ));

    core.storage
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
        core.ingest(media_event(altered)).await,
        Err(CoreError::MediaMetadataMismatch(id)) if id == attachment.media_id
    ));

    assert_eq!(
        core.ingest(media_event(attachment))
            .await
            .unwrap()
            .dispatches()
            .len(),
        1
    );
}

fn edit_event(
    connector: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    native_id: &str,
    text: Option<&str>,
) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(connector.into()),
            address: ChatAddress::new(chat_id, thread_id.map(str::to_owned)),
            chat_title: None,
        },
        payload: EventKind::Edit {
            native_id: native_id.into(),
            text: text.map(str::to_owned),
        },
    }
}

#[tokio::test]
async fn edit_after_delivery_revises_the_delivered_copy() {
    let core = core(vec![direct_rule()]);
    let IngestOutcome::Routed {
        message_id,
        dispatches,
        ..
    } = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap()
    else {
        panic!("create should route");
    };
    let telegram = dispatches[0].to.endpoint_id;
    core.record_dispatched_copy(message_id, telegram, "tg-1")
        .await
        .unwrap();

    let edited = core
        .ingest(edit_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            None,
            "wa-1",
            Some("hello edited"),
        ))
        .await
        .unwrap();
    let dispatch = &edited.dispatches()[0];
    assert_eq!(dispatch.to.connector_id, ConnectorId(TELEGRAM.into()));
    let Action::Edit {
        message, native_id, ..
    } = &dispatch.action
    else {
        panic!("edit should route as a message edit");
    };
    assert_eq!(native_id, "tg-1");
    assert_eq!(message.text.as_deref(), Some("hello edited"));
}

#[tokio::test]
async fn edit_before_delivery_blocks_then_releases_with_native_id() {
    let core = core(vec![direct_rule()]);
    let IngestOutcome::Routed { dispatches, .. } = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap()
    else {
        panic!("create should route");
    };
    let create_dispatch = dispatches[0].clone();

    let edited = core
        .ingest(edit_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            None,
            "wa-1",
            Some("edited before delivery"),
        ))
        .await
        .unwrap();
    let IngestOutcome::Routed {
        blocked,
        dispatches,
        ..
    } = edited
    else {
        panic!("edit should be stored while delivery is blocked");
    };
    assert!(dispatches.is_empty());
    assert_eq!(blocked, 1);

    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: create_dispatch.id,
            native_id: "tg-1".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    let edit = released
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Edit { .. }))
        .expect("acknowledging the create releases the blocked edit");
    let Action::Edit { native_id, .. } = &edit.action else {
        unreachable!();
    };
    assert_eq!(native_id, "tg-1");
}

fn reaction_event(connector: &str, chat_id: &str, native_id: &str, emoji: Option<&str>) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(connector.into()),
            address: ChatAddress::new(chat_id, None),
            chat_title: None,
        },
        payload: EventKind::Reaction {
            native_id: native_id.into(),
            reactor: "reactor-1".into(),
            reactor_name: Some("Alice".into()),
            emoji: emoji.map(str::to_owned),
            from_me: false,
        },
    }
}

fn receipt_event(
    connector: &str,
    chat_id: &str,
    native_id: &str,
    kind: ReceiptKind,
    from_me: bool,
) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(connector.into()),
            address: ChatAddress::new(chat_id, None),
            chat_title: None,
        },
        payload: EventKind::Receipt {
            native_id: native_id.into(),
            observer: "peer".into(),
            from_me,
            kind,
        },
    }
}

#[tokio::test]
async fn reaction_after_delivery_fans_to_the_delivered_copy() {
    let core = core(vec![direct_rule()]);
    let IngestOutcome::Routed {
        message_id,
        dispatches,
        ..
    } = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap()
    else {
        panic!("create should route");
    };
    let telegram = dispatches[0].to.endpoint_id;
    core.record_dispatched_copy(message_id, telegram, "tg-1")
        .await
        .unwrap();

    let reacted = core
        .ingest(reaction_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-1",
            Some("❤️"),
        ))
        .await
        .unwrap();
    let dispatch = &reacted.dispatches()[0];
    assert_eq!(dispatch.to.connector_id, ConnectorId(TELEGRAM.into()));
    let Action::React {
        native_id,
        emoji,
        reactor_name,
        ..
    } = &dispatch.action
    else {
        panic!("reaction should route as a reaction dispatch");
    };
    assert_eq!(native_id, "tg-1");
    assert_eq!(emoji.as_deref(), Some("❤️"));
    assert_eq!(reactor_name.as_deref(), Some("Alice"));
}

#[tokio::test]
async fn reaction_before_delivery_blocks_then_releases_with_native_id() {
    let core = core(vec![direct_rule()]);
    let IngestOutcome::Routed { dispatches, .. } = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap()
    else {
        panic!("create should route");
    };
    let create_dispatch = dispatches[0].clone();

    let reacted = core
        .ingest(reaction_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-1",
            Some("👍"),
        ))
        .await
        .unwrap();
    let IngestOutcome::Routed {
        blocked,
        dispatches,
        ..
    } = reacted
    else {
        panic!("reaction should be stored while delivery is blocked");
    };
    assert!(dispatches.is_empty());
    assert_eq!(blocked, 1);

    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: create_dispatch.id,
            native_id: "tg-1".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    let reaction = released
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::React { .. }))
        .expect("acknowledging the create releases the blocked reaction");
    let Action::React {
        native_id, emoji, ..
    } = &reaction.action
    else {
        unreachable!();
    };
    assert_eq!(native_id, "tg-1");
    assert_eq!(emoji.as_deref(), Some("👍"));
}

#[tokio::test]
async fn acknowledging_a_mirror_create_emits_a_delivery_ack_to_the_origin() {
    let core = core(vec![direct_rule()]);
    let IngestOutcome::Routed { dispatches, .. } = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap()
    else {
        panic!("create should route");
    };
    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: dispatches[0].id,
            native_id: "tg-1".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    let ack = released
        .iter()
        .find(|dispatch| {
            matches!(
                dispatch.action,
                Action::Receipt {
                    kind: ReceiptKind::Delivered,
                    ..
                }
            )
        })
        .expect("acknowledging the mirror create emits a delivery ack");
    assert_eq!(ack.to.connector_id, ConnectorId(WHATSAPP.into()));
    let Action::Receipt {
        kind: ReceiptKind::Delivered,
        native_id,
        ..
    } = &ack.action
    else {
        unreachable!();
    };
    assert_eq!(native_id, "wa-1");
}

#[tokio::test]
async fn read_receipt_on_a_mirror_copy_surfaces_a_seen_receipt_on_the_origin() {
    let core = core(vec![direct_rule()]);
    let IngestOutcome::Routed {
        message_id,
        dispatches,
        ..
    } = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap()
    else {
        panic!("create should route");
    };
    let telegram = dispatches[0].to.endpoint_id;
    core.record_dispatched_copy(message_id, telegram, "tg-1")
        .await
        .unwrap();

    let seen = core
        .ingest(Event {
            source: SourceAddress {
                connector_id: ConnectorId(TELEGRAM.into()),
                address: ChatAddress::new("-100123", Some("12".into())),
                chat_title: None,
            },
            payload: EventKind::Receipt {
                native_id: "tg-1".into(),
                observer: "peer".into(),
                from_me: false,
                kind: ReceiptKind::Read,
            },
        })
        .await
        .unwrap();
    let dispatch = &seen.dispatches()[0];
    assert_eq!(dispatch.to.connector_id, ConnectorId(WHATSAPP.into()));
    let Action::Receipt {
        kind: ReceiptKind::Read,
        native_id,
        ..
    } = &dispatch.action
    else {
        panic!("read receipt should surface a seen receipt");
    };
    assert_eq!(native_id, "wa-1");
}

#[tokio::test]
async fn receipts_for_a_message_sent_from_another_source_device_surface_on_the_mirror() {
    let core = core(vec![direct_rule()]);
    // The owner sent this from another WhatsApp device; it arrives at the source
    // and is mirrored to Telegram, where the Telegram copy is a sent (not origin)
    // copy. A peer delivery/read receipt is observed at the WhatsApp endpoint.
    let IngestOutcome::Routed {
        message_id,
        dispatches,
        ..
    } = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap()
    else {
        panic!("create should route");
    };
    let telegram = dispatches[0].to.endpoint_id;
    core.record_dispatched_copy(message_id, telegram, "tg-1")
        .await
        .unwrap();

    let delivered = core
        .ingest(receipt_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-1",
            ReceiptKind::Delivered,
            false,
        ))
        .await
        .unwrap();
    let ack = &delivered.dispatches()[0];
    assert_eq!(ack.to.connector_id, ConnectorId(TELEGRAM.into()));
    let Action::Receipt {
        kind: ReceiptKind::Delivered,
        native_id,
        ..
    } = &ack.action
    else {
        panic!("delivered receipt should surface a delivery ack on the mirror");
    };
    assert_eq!(native_id, "tg-1");

    let read = core
        .ingest(receipt_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-1",
            ReceiptKind::Read,
            false,
        ))
        .await
        .unwrap();
    let seen = &read.dispatches()[0];
    assert_eq!(seen.to.connector_id, ConnectorId(TELEGRAM.into()));
    let Action::Receipt {
        kind: ReceiptKind::Read,
        native_id,
        ..
    } = &seen.action
    else {
        panic!("read receipt should surface a seen receipt on the mirror");
    };
    assert_eq!(native_id, "tg-1");
}

#[tokio::test]
async fn own_receipt_is_ignored() {
    let core = core(vec![direct_rule()]);
    assert_eq!(
        core.ingest(receipt_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-1",
            ReceiptKind::Read,
            true,
        ))
        .await
        .unwrap(),
        IngestOutcome::Ignored(IgnoreReason::Unmatched),
    );
}

#[tokio::test]
async fn reaction_for_unknown_message_is_ignored() {
    let core = core(vec![direct_rule()]);
    assert_eq!(
        core.ingest(reaction_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            "wa-unknown",
            Some("❤️"),
        ))
        .await
        .unwrap(),
        IngestOutcome::Ignored(IgnoreReason::Unmatched),
    );
}

#[tokio::test]
async fn edit_that_does_not_change_text_is_ignored() {
    let core = core(vec![direct_rule()]);
    core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(
        core.ingest(edit_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            None,
            "wa-1",
            Some("hello"),
        ))
        .await
        .unwrap(),
        IngestOutcome::Ignored(IgnoreReason::RedundantEdit)
    );
}

#[tokio::test]
async fn edit_for_unknown_message_is_ignored() {
    let core = core(vec![direct_rule()]);
    assert_eq!(
        core.ingest(edit_event(
            WHATSAPP,
            "12345@s.whatsapp.net",
            None,
            "wa-unknown",
            Some("edited"),
        ))
        .await
        .unwrap(),
        IngestOutcome::Ignored(IgnoreReason::Unmatched)
    );
}

fn owner_message(chat_id: &str, text: &str, reply_to: Option<&str>) -> OwnerMessage {
    OwnerMessage {
        connector_id: ConnectorId(WHATSAPP.into()),
        chat_id: chat_id.into(),
        thread_id: None,
        text: text.into(),
        reply_to_native_id: reply_to.map(str::to_owned),
        timestamp_ms: 5,
    }
}

// The phase-5 STOP gate: an owner message must never loop. The acknowledgement
// records a sent copy, so the platform reporting our own message back as an
// inbound event is dropped as an echo instead of being ingested again.
#[tokio::test]
async fn owner_message_echo_is_suppressed_after_acknowledgement() {
    // No rules: the owner message mints the chat's topology itself.
    let core = core(vec![]);
    let outcome = core
        .send_owner_message(owner_message("999@s.whatsapp.net", "hi there", None))
        .await
        .unwrap();
    assert!(outcome.mirrored.is_empty());
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: outcome.direct.id,
        native_id: "wa-raw-1".into(),
        thread_id: None,
    })
    .await
    .unwrap();
    assert_eq!(
        core.ingest(event(WHATSAPP, "999@s.whatsapp.net", "wa-raw-1", None))
            .await
            .unwrap(),
        IngestOutcome::Ignored(IgnoreReason::SentMessageEcho)
    );
}

#[tokio::test]
async fn owner_message_to_a_routed_chat_fans_out_to_the_other_side() {
    let core = core(vec![direct_rule()]);
    let outcome = core
        .send_owner_message(owner_message("12345@s.whatsapp.net", "from the tui", None))
        .await
        .unwrap();
    assert_eq!(outcome.direct.to.connector_id, ConnectorId(WHATSAPP.into()));
    assert_eq!(outcome.mirrored.len(), 1);
    let mirror = &outcome.mirrored[0].to;
    assert_eq!(mirror.connector_id, ConnectorId(TELEGRAM.into()));
    assert_eq!(mirror.address.chat_id, "-100123");

    // The echo stays suppressed on the routed path too.
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: outcome.direct.id,
        native_id: "wa-raw-2".into(),
        thread_id: None,
    })
    .await
    .unwrap();
    assert_eq!(
        core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-raw-2", None))
            .await
            .unwrap(),
        IngestOutcome::Ignored(IgnoreReason::SentMessageEcho)
    );
}

#[tokio::test]
async fn owner_message_to_an_unrouted_chat_gets_a_rule_less_conversation() {
    let storage = Arc::new(InMemoryStorage::default());
    let core = core_over(Arc::clone(&storage), vec![direct_rule()]);
    let outcome = core
        .send_owner_message(owner_message("unrouted@s.whatsapp.net", "hello", None))
        .await
        .unwrap();
    assert!(outcome.mirrored.is_empty());
    // The chat gets a conversation of its own with no rule behind it.
    let endpoint_id = outcome.direct.to.endpoint_id;
    let conversation = storage
        .conversation_for_endpoint(endpoint_id)
        .await
        .unwrap()
        .expect("the standalone chat has a conversation");
    assert_eq!(conversation.rule_id, None);
    assert_eq!(conversation.endpoints, vec![endpoint_id]);
    // A later peer message in the standalone chat resolves cleanly and fans
    // out to nobody, rather than being dropped as no longer covered by a rule.
    match core
        .ingest(event(WHATSAPP, "unrouted@s.whatsapp.net", "wa-5", None))
        .await
        .unwrap()
    {
        IngestOutcome::Routed { dispatches, .. } => assert!(dispatches.is_empty()),
        other => panic!("inbound after an owner message should ingest, got {other:?}"),
    }
}

#[tokio::test]
async fn owner_message_reply_quotes_the_stored_parent_and_degrades_to_plain() {
    let core = core(vec![direct_rule()]);
    core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-parent", None))
        .await
        .unwrap();
    let outcome = core
        .send_owner_message(owner_message(
            "12345@s.whatsapp.net",
            "replying",
            Some("wa-parent"),
        ))
        .await
        .unwrap();
    let Action::Send {
        message,
        reply: reply_context,
        ..
    } = &outcome.direct.action
    else {
        panic!("owner message dispatches a message create");
    };
    assert!(message.reply_to.is_some());
    assert_eq!(message.author_id, OWNER_AUTHOR);
    let context = reply_context.as_ref().expect("stored parent quotes");
    assert_eq!(context.native_id, "wa-parent");
    assert_eq!(context.author_id, "alice");

    // A parent the bridge never stored degrades to a plain send.
    let outcome = core
        .send_owner_message(owner_message(
            "12345@s.whatsapp.net",
            "plain",
            Some("ghost"),
        ))
        .await
        .unwrap();
    let Action::Send {
        message,
        reply: reply_context,
        ..
    } = &outcome.direct.action
    else {
        panic!("owner message dispatches a message create");
    };
    assert!(message.reply_to.is_none() && reply_context.is_none());
}

#[tokio::test]
async fn owner_message_to_an_unknown_connector_is_rejected() {
    let core = core(vec![]);
    let error = core
        .send_owner_message(OwnerMessage {
            connector_id: ConnectorId("unregistered".into()),
            chat_id: "somewhere".into(),
            thread_id: None,
            text: "hi".into(),
            reply_to_native_id: None,
            timestamp_ms: 5,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::UnknownConnector(_)));
}

/// A many-to-one rule whose allow list names the source chat by its phone JID,
/// the form a user can actually read off their contact list. WhatsApp delivers
/// the same chat under a LID.
fn phone_listed_mirror_rule(filter: ChatFilter) -> Rule {
    Rule {
        id: RuleId("wa-mirror".into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chats_filter: filter,
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress {
                chat_id: "-1001234567".into(),
                thread_id: None,
            },
        },
    }
}

fn chats(chat_id: &str) -> Vec<ChatAddress> {
    vec![ChatAddress {
        chat_id: chat_id.into(),
        thread_id: None,
    }]
}

async fn link_phone_to_lid(core: &Core<InMemoryStorage>) {
    core.migrate_identity(
        ConnectorId(WHATSAPP.into()),
        ChatAddress::new("15550001111@s.whatsapp.net", None),
        ChatAddress::new("88887777@lid", None),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn allow_list_written_with_the_phone_jid_matches_the_linked_lid() {
    let core = core(vec![phone_listed_mirror_rule(ChatFilter::Only {
        chats: chats("15550001111@s.whatsapp.net"),
    })]);
    // The connector reports the phone/LID equivalence before the message, as it
    // does for every direct chat it resolves.
    link_phone_to_lid(&core).await;

    let outcome = core
        .ingest(event(WHATSAPP, "88887777@lid", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(outcome.dispatches().len(), 1);
}

#[tokio::test]
async fn deny_list_written_with_the_phone_jid_excludes_the_linked_lid() {
    let core = core(vec![phone_listed_mirror_rule(ChatFilter::Not {
        chats: chats("15550001111@s.whatsapp.net"),
    })]);
    link_phone_to_lid(&core).await;

    let outcome = core
        .ingest(event(WHATSAPP, "88887777@lid", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
}

#[tokio::test]
async fn an_unlinked_chat_is_unaffected_by_another_chats_link() {
    let core = core(vec![phone_listed_mirror_rule(ChatFilter::Only {
        chats: chats("15550001111@s.whatsapp.net"),
    })]);
    link_phone_to_lid(&core).await;

    let outcome = core
        .ingest(event(WHATSAPP, "99999999@lid", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
}

#[tokio::test]
async fn every_known_form_of_a_matched_chat_resolves_to_one_endpoint() {
    let core = core(vec![phone_listed_mirror_rule(ChatFilter::Only {
        chats: chats("15550001111@s.whatsapp.net"),
    })]);
    link_phone_to_lid(&core).await;
    let lid = core
        .ingest(event(WHATSAPP, "88887777@lid", "wa-1", None))
        .await
        .unwrap();
    let IngestOutcome::Routed { message_id, .. } = lid else {
        panic!("listed chat should route");
    };

    // The same chat arriving under the phone JID lands in the conversation the
    // LID form already established, rather than minting a second one with its
    // own mirror thread.
    let phone = core
        .ingest(event(WHATSAPP, "15550001111@s.whatsapp.net", "wa-2", None))
        .await
        .unwrap();
    let IngestOutcome::Deferred {
        message_id: deferred,
    } = phone
    else {
        panic!("second message should be held for the in-flight provision");
    };
    assert_ne!(message_id, deferred);
}

#[tokio::test]
async fn address_forms_chain_across_successive_migrations() {
    let core = core(vec![phone_listed_mirror_rule(ChatFilter::Only {
        chats: chats("15550001111@s.whatsapp.net"),
    })]);
    link_phone_to_lid(&core).await;
    // A second hop: the closure has to walk phone -> lid -> new lid for the
    // rule's phone-number entry to still name the chat.
    core.migrate_identity(
        ConnectorId(WHATSAPP.into()),
        ChatAddress::new("88887777@lid", None),
        ChatAddress::new("66665555@lid", None),
    )
    .await
    .unwrap();

    let outcome = core
        .ingest(event(WHATSAPP, "66665555@lid", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(outcome.dispatches().len(), 1);
}

#[tokio::test]
async fn a_form_learned_after_the_chat_is_routed_joins_the_same_conversation() {
    let core = core(vec![phone_listed_mirror_rule(ChatFilter::All)]);
    let first = core
        .ingest(event(WHATSAPP, "88887777@lid", "wa-1", None))
        .await
        .unwrap();
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: first.dispatches()[0].id,
        native_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();

    // Mid-run, the connector learns the phone number behind the LID. Nothing
    // migrated — the chat is already routed under the form it arrived on — but
    // the newly learned form has to reach the same conversation.
    link_phone_to_lid(&core).await;

    let under_phone = core
        .ingest(event(WHATSAPP, "15550001111@s.whatsapp.net", "wa-2", None))
        .await
        .unwrap();
    let dispatch = &under_phone.dispatches()[0];
    assert_eq!(
        dispatch.to.address,
        ChatAddress::new("-1001234567", Some("777".into()))
    );
    assert!(
        dispatch.to.create_thread.is_none(),
        "the phone form provisioned a second mirror thread"
    );
}

#[tokio::test]
async fn a_deny_list_applies_to_a_chat_once_its_listed_form_is_learned() {
    let core = core(vec![phone_listed_mirror_rule(ChatFilter::Not {
        chats: chats("15550001111@s.whatsapp.net"),
    })]);
    // The chat arrives before the core knows the phone number behind it, so
    // nothing yet connects it to the deny list and it routes.
    let leaked = core
        .ingest(event(WHATSAPP, "88887777@lid", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(leaked.dispatches().len(), 1);

    link_phone_to_lid(&core).await;

    // Now that the forms are known to be one chat, the deny list covers it and
    // replication stops, rather than the chat staying bridged because it once
    // slipped past.
    let outcome = core
        .ingest(event(WHATSAPP, "88887777@lid", "wa-2", None))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
}

#[tokio::test]
async fn a_routed_chat_stops_when_its_rule_leaves_the_config() {
    let storage = Arc::new(InMemoryStorage::default());
    let core = core_over(
        Arc::clone(&storage),
        vec![phone_listed_mirror_rule(ChatFilter::All)],
    );
    let routed = core
        .ingest(event(WHATSAPP, "88887777@lid", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(routed.dispatches().len(), 1);

    // Restart with the rule removed: the endpoints it minted are still in
    // storage, but nothing declares them any more.
    let restarted = core_over(storage, vec![]);
    let outcome = restarted
        .ingest(event(WHATSAPP, "88887777@lid", "wa-2", None))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
}

#[tokio::test]
async fn migration_heals_a_chat_split_across_the_observation_log() {
    let storage = Arc::new(InMemoryStorage::default());
    let core = core_over(Arc::clone(&storage), vec![]);
    let connector = ConnectorId(WHATSAPP.into());
    let phone = "15550001111@s.whatsapp.net";
    let lid = "88887777@lid";

    // The observation log records chats no rule matches, so it logs under
    // whichever address the connector reported before it knew the two were one.
    for (chat_id, native_id, ts) in [(phone, "m-1", 10), (lid, "m-2", 20)] {
        storage
            .log_observed(
                wuphf_model::ObservedMessage {
                    connector_id: connector.clone(),
                    platform: Platform::WhatsApp,
                    chat_id: chat_id.into(),
                    thread_id: None,
                    native_id: native_id.into(),
                    author_id: "alice".into(),
                    author_name: None,
                    participant: None,
                    text: Some("hi".into()),
                    attachments: Vec::new(),
                    reply_to_native_id: None,
                    direction: wuphf_model::ObservedDirection::Incoming,
                    timestamp_ms: ts,
                    deleted: false,
                    edited: false,
                },
                None,
            )
            .await
            .unwrap();
    }
    assert_eq!(storage.list_observed_chats().await.unwrap().len(), 2);

    core.migrate_identity(
        connector.clone(),
        ChatAddress::new(phone, None),
        ChatAddress::new(lid, None),
    )
    .await
    .unwrap();

    let chats = storage.list_observed_chats().await.unwrap();
    assert_eq!(chats.len(), 1, "the observer should show one chat, not two");
    assert_eq!(chats[0].chat_id, lid);
    assert_eq!(
        storage
            .observed_messages(&connector, lid, None, 10)
            .await
            .unwrap()
            .len(),
        2,
    );
}

#[tokio::test]
async fn a_quote_of_a_received_copy_is_not_attributed_to_the_bridge() {
    let core = core(vec![direct_rule()]);
    // A message from the WhatsApp side, mirrored to Telegram.
    let inbound = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: inbound.dispatches()[0].id,
        native_id: "tg-1".into(),
        thread_id: None,
    })
    .await
    .unwrap();

    // Replying to it from Telegram sends back to WhatsApp, quoting the copy
    // WhatsApp itself gave us — so the quote belongs to the peer, not to us.
    let reply = core
        .ingest(event_in(
            TELEGRAM,
            "-100123",
            Some("12"),
            "tg-2",
            Some("tg-1"),
        ))
        .await
        .unwrap();
    let Action::Send {
        reply: reply_context,
        ..
    } = &reply.dispatches()[0].action
    else {
        panic!("reply should dispatch a create");
    };
    let reply_context = reply_context.as_ref().expect("the reply quotes its parent");
    assert_eq!(reply_context.native_id, "wa-1");
    assert!(
        !reply_context.authored_by_bridge,
        "a copy received from the target platform is not bridge-authored",
    );
}

#[tokio::test]
async fn a_quote_of_a_bridge_sent_copy_is_attributed_to_the_bridge() {
    let core = core(vec![direct_rule()]);
    // A message from Telegram, mirrored into WhatsApp by the bridge.
    let inbound = core
        .ingest(event_in(TELEGRAM, "-100123", Some("12"), "tg-1", None))
        .await
        .unwrap();
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: inbound.dispatches()[0].id,
        native_id: "wa-1".into(),
        thread_id: None,
    })
    .await
    .unwrap();

    // Replying on Telegram quotes the WhatsApp copy the bridge wrote.
    let reply = core
        .ingest(event_in(
            TELEGRAM,
            "-100123",
            Some("12"),
            "tg-2",
            Some("tg-1"),
        ))
        .await
        .unwrap();
    let Action::Send {
        reply: reply_context,
        ..
    } = &reply.dispatches()[0].action
    else {
        panic!("reply should dispatch a create");
    };
    let reply_context = reply_context.as_ref().expect("the reply quotes its parent");
    assert_eq!(reply_context.native_id, "wa-1");
    assert!(reply_context.authored_by_bridge);
}

/// A core whose owner notices go to a Telegram chat, plus a WhatsApp target the
/// notices must skip when WhatsApp is the connector in trouble.
fn notifying_core() -> Core<InMemoryStorage> {
    let config = RoutingConfig::new(vec![], connectors()).with_notice_targets(vec![
        wuphf_model::ChatRef {
            connector_id: ConnectorId(TELEGRAM.into()),
            address: ChatAddress::new("-100999", None),
        },
        wuphf_model::ChatRef {
            connector_id: ConnectorId(WHATSAPP.into()),
            address: ChatAddress::new("99999@s.whatsapp.net", None),
        },
    ]);
    Core::new(Arc::new(InMemoryStorage::default()), Arc::new(config))
}

fn notice_text(dispatch: &Dispatch) -> String {
    let Action::Send { message, .. } = &dispatch.action else {
        panic!("a notice should dispatch a message create");
    };
    message.text.clone().expect("a notice carries text")
}

#[tokio::test]
async fn an_owner_notice_reaches_the_other_connectors_only() {
    let core = notifying_core();
    let dispatches = core
        .notify_owners(
            ConnectorId(WHATSAPP.into()),
            NoticeKind::LoggedOut,
            "WhatsApp ended the session",
            1,
        )
        .await
        .unwrap();

    // The WhatsApp target is skipped: a logged-out connector cannot deliver
    // the news that it is logged out.
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(TELEGRAM.into()));
    let text = notice_text(&dispatches[0]);
    assert!(
        text.contains(WHATSAPP),
        "the notice names the connector: {text}"
    );
    assert!(
        text.contains("re-pair"),
        "the notice says what to do: {text}"
    );
    assert!(text.contains("WhatsApp ended the session"));
}

#[tokio::test]
async fn a_repeated_notice_is_sent_once_until_the_state_changes() {
    let core = notifying_core();
    let whatsapp = ConnectorId(WHATSAPP.into());

    let first = core
        .notify_owners(whatsapp.clone(), NoticeKind::LoggedOut, "", 1)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);

    // A connector in a reconnect loop repeats itself; the owner is told once.
    let repeat = core
        .notify_owners(whatsapp.clone(), NoticeKind::LoggedOut, "", 2)
        .await
        .unwrap();
    assert!(repeat.is_empty());

    // A change is news again, and reads as a recovery.
    let recovered = core
        .notify_owners(whatsapp, NoticeKind::Recovered, "", 3)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert!(notice_text(&recovered[0]).contains("working again"));
}

#[tokio::test]
async fn a_recovery_without_prior_trouble_says_nothing() {
    let core = notifying_core();
    // Connectors report health on every connect, so a healthy start-up must not
    // announce that nothing is wrong.
    let dispatches = core
        .notify_owners(ConnectorId(WHATSAPP.into()), NoticeKind::Recovered, "", 1)
        .await
        .unwrap();
    assert!(dispatches.is_empty());
}

#[tokio::test]
async fn a_notice_with_no_configured_target_is_dropped() {
    let core = core(vec![]);
    let dispatches = core
        .notify_owners(ConnectorId(WHATSAPP.into()), NoticeKind::LoggedOut, "", 1)
        .await
        .unwrap();
    assert!(dispatches.is_empty());
}
