use super::*;
use crate::ChatStateSettings;
use std::collections::{HashMap, HashSet};
use wuphf_model::{
    Action, Attachment, AttachmentKind, ChatAddress, ChatFilter, ChatRef, ChatUpdate, Connector,
    ConnectorCapabilities, DisappearingTimer, Dispatch, DispatchAck, IgnoreReason, MediaBlob,
    MediaId, MessageAuthor, NewMessage, NoticeKind, OWNER_AUTHOR, OwnerMessage, Platform,
    ReactorReaction, ReceiptKind, Rule, RuleId, RuleKind, SourceAddress,
};
use wuphf_storage::{
    ChatProfileStore, InMemoryStorage, MediaStore, MessageStore, ObservationStore, TopologyStore,
};
use wuphf_storage::{DispatchStore, HealthStore};

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
                supports_disappearing_timers: false,
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
            disappearing: None,
            author_account: None,
            reply_to_chat_id: None,
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

    // The first source message provisions the mirror topic, through the
    // conversation's info card.
    let first = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    let IngestOutcome::Routed { dispatches, .. } = first else {
        panic!("first message routes");
    };
    assert_eq!(dispatches.len(), 1);
    assert!(dispatches[0].to.create_thread.is_some());
    assert!(matches!(dispatches[0].action, Action::Note { .. }));

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

    // A provisions the mirror thread (through the info card, A itself waiting
    // in the stash); B arrives mid-provision and is deferred into the stash.
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

    // Failing the provisioning dispatch (the info card) must release the
    // stash rather than strand the conversation: the first stashed message
    // replays and re-attempts provisioning (a fresh create still carrying a
    // create_thread, since no thread exists). The second waits behind it
    // rather than provisioning a second thread.
    let released = core
        .fail_dispatch(provisioning_id, "cannot create topic".into())
        .await
        .unwrap();
    let sends: Vec<_> = released
        .iter()
        .filter(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .collect();
    assert_eq!(sends.len(), 1, "one replay provisions, the other waits");
    assert!(
        sends[0].to.create_thread.is_some(),
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

/// Two accounts in one WhatsApp group each report every message under the same
/// native id. Each report is its own event, bridged by its own account's rule;
/// only a repeat from the same account is a duplicate.
#[tokio::test]
async fn two_accounts_in_one_group_each_bridge_the_same_message() {
    const SECOND: &str = "whatsapp-second";
    let rule = |id: &str, source: &str, destination: &str| Rule {
        id: RuleId(id.into()),
        flow: RuleKind::OneToOne {
            source_connector: ConnectorId(source.into()),
            source_chat: ChatAddress::new("111@g.us", None),
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress::new(destination, Some("1".into())),
        },
    };
    let mut connectors = connectors();
    connectors.push(Connector {
        id: ConnectorId(SECOND.into()),
        platform: Platform::WhatsApp,
        capabilities: ConnectorCapabilities::default(),
    });
    let config = Arc::new(RoutingConfig::new(
        vec![
            rule("first", WHATSAPP, "-100111"),
            rule("second", SECOND, "-100222"),
        ],
        connectors,
    ));
    let core = Core::new(Arc::new(InMemoryStorage::default()), config);

    for (connector, destination) in [(WHATSAPP, "-100111"), (SECOND, "-100222")] {
        let outcome = core
            .ingest(event(connector, "111@g.us", "wa-1", None))
            .await
            .unwrap();
        let dispatches = outcome.dispatches();
        assert_eq!(dispatches.len(), 1, "{connector}");
        assert_eq!(dispatches[0].to.address.chat_id, destination);
    }
    assert_eq!(
        core.ingest(event(SECOND, "111@g.us", "wa-1", None))
            .await
            .unwrap(),
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
        media_ids: Vec::new(),
        client: None,
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

    // The mirror labels the copy as the owner's; the owner's own account posts
    // the direct copy, which needs no label.
    let sent = |dispatch: &Dispatch| match &dispatch.action {
        Action::Send { message, .. } => (message.participant.clone(), message.author_name.clone()),
        other => panic!("owner message dispatches a send, got {other:?}"),
    };
    assert_eq!(
        sent(&outcome.mirrored[0]),
        (Some(OWNER_AUTHOR.to_owned()), Some(OWNER_AUTHOR.to_owned()))
    );
    assert_eq!(sent(&outcome.direct), (None, None));

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
async fn owner_message_media_ids_carry_the_stored_attachments_to_every_copy() {
    let core = core(vec![direct_rule()]);
    let mut stored = Vec::new();
    for (kind, name) in [
        (AttachmentKind::Video, "reel.mp4"),
        (AttachmentKind::Image, "2.jpg"),
    ] {
        let attachment = Attachment {
            media_id: MediaId::new(),
            kind,
            file_name: Some(name.into()),
            mime_type: None,
            size_bytes: Some(3),
            animated: false,
        };
        core.storage
            .put_media(
                MediaBlob {
                    attachment: attachment.clone(),
                    bytes: b"abc".to_vec(),
                },
                1,
            )
            .await
            .unwrap();
        stored.push(attachment);
    }
    let mut outbound = owner_message("12345@s.whatsapp.net", "", None);
    outbound.media_ids = stored
        .iter()
        .map(|attachment| attachment.media_id)
        .collect();
    outbound.client = Some("wuphf-misc".into());
    let outcome = core.send_owner_message(outbound).await.unwrap();

    let sent = |dispatch: &Dispatch| match &dispatch.action {
        Action::Send { message, .. } => (message.text.clone(), message.attachments.clone()),
        other => panic!("owner message dispatches a send, got {other:?}"),
    };
    // Media only: no empty text for a connector to render as a caption.
    assert_eq!(sent(&outcome.direct), (None, stored.clone()));
    assert_eq!(outcome.mirrored.len(), 1);
    assert_eq!(sent(&outcome.mirrored[0]), (None, stored));
}

#[tokio::test]
async fn owner_message_with_an_unknown_media_id_is_rejected() {
    let core = core(vec![direct_rule()]);
    let missing = MediaId::new();
    let mut outbound = owner_message("12345@s.whatsapp.net", "caption", None);
    outbound.media_ids = vec![missing];
    assert!(matches!(
        core.send_owner_message(outbound).await,
        Err(CoreError::UnknownMedia(id)) if id == missing
    ));
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
            media_ids: Vec::new(),
            client: None,
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
                    sent_by: None,
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
            &[],
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
async fn an_announcement_reaches_every_notice_target_each_time() {
    let core = notifying_core();
    let announce = || core.announce("⬆️ wuphf 0.2.0 is out", 1);
    let dispatches = announce().await.unwrap();

    // Not about a connector: none is skipped, and the text is the caller's.
    let mut reached: Vec<_> = dispatches
        .iter()
        .map(|dispatch| dispatch.to.connector_id.0.as_str())
        .collect();
    reached.sort();
    assert_eq!(reached, [TELEGRAM, WHATSAPP]);
    assert_eq!(notice_text(&dispatches[0]), "⬆️ wuphf 0.2.0 is out");
    // The caller decides what repeats.
    assert_eq!(announce().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_link_notice_shows_its_stored_qr_and_every_round_is_sent() {
    let core = notifying_core();
    let whatsapp = ConnectorId(WHATSAPP.into());
    let qr = Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::Image,
        file_name: Some("qr.png".into()),
        mime_type: Some("image/png".into()),
        size_bytes: Some(3),
        animated: false,
    };
    core.storage
        .put_media(
            MediaBlob {
                attachment: qr.clone(),
                bytes: b"png".to_vec(),
            },
            1,
        )
        .await
        .unwrap();
    // An id the store does not hold is left out; the text still goes.
    let media = [qr.media_id, MediaId::new()];
    let notify = || {
        core.notify_owners(
            whatsapp.clone(),
            NoticeKind::LinkRequired,
            "scan the QR code",
            &media,
            1,
        )
    };
    let first = notify().await.unwrap();
    assert_eq!(first.len(), 1);
    let Action::Send { message, .. } = &first[0].action else {
        panic!("a notice should dispatch a message create");
    };
    assert_eq!(message.attachments, vec![qr]);
    assert!(notice_text(&first[0]).contains("scan the QR code"));
    // Not a health state: the next pairing round is news too.
    assert_eq!(notify().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_repeated_notice_is_sent_once_until_the_state_changes() {
    let core = notifying_core();
    let whatsapp = ConnectorId(WHATSAPP.into());

    let first = core
        .notify_owners(whatsapp.clone(), NoticeKind::LoggedOut, "", &[], 1)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);

    // A connector in a reconnect loop repeats itself; the owner is told once.
    let repeat = core
        .notify_owners(whatsapp.clone(), NoticeKind::LoggedOut, "", &[], 2)
        .await
        .unwrap();
    assert!(repeat.is_empty());

    // A change is news again, and reads as a recovery.
    let recovered = core
        .notify_owners(whatsapp, NoticeKind::Recovered, "", &[], 3)
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
        .notify_owners(
            ConnectorId(WHATSAPP.into()),
            NoticeKind::Recovered,
            "",
            &[],
            1,
        )
        .await
        .unwrap();
    assert!(dispatches.is_empty());
}

#[tokio::test]
async fn a_notice_with_no_configured_target_is_dropped() {
    let core = core(vec![]);
    let dispatches = core
        .notify_owners(
            ConnectorId(WHATSAPP.into()),
            NoticeKind::LoggedOut,
            "",
            &[],
            1,
        )
        .await
        .unwrap();
    assert!(dispatches.is_empty());
}

fn connector_ref(connector: &str, chat_id: &str) -> ChatRef {
    ChatRef {
        connector_id: ConnectorId(connector.into()),
        address: ChatAddress::new(chat_id, None),
    }
}

#[test]
fn validate_rejects_unknown_rule_connector() {
    let mut rule = direct_rule();
    let RuleKind::OneToOne {
        destination_connector,
        ..
    } = &mut rule.flow
    else {
        unreachable!("direct_rule is one-to-one");
    };
    *destination_connector = ConnectorId("discord-main".into());
    let config = RoutingConfig::new(vec![rule], connectors());
    assert_eq!(
        config.validate(),
        Err(RoutingConfigError::UnknownRuleConnector {
            rule: RuleId("direct".into()),
            connector: ConnectorId("discord-main".into()),
        })
    );
}

#[test]
fn validate_rejects_duplicate_rule_ids() {
    let mut second = mirror_rule();
    second.id = RuleId("direct".into());
    let config = RoutingConfig::new(vec![direct_rule(), second], connectors());
    assert_eq!(
        config.validate(),
        Err(RoutingConfigError::DuplicateRule(RuleId("direct".into())))
    );
}

#[test]
fn validate_rejects_unknown_notice_connector() {
    let config = RoutingConfig::new(vec![direct_rule()], connectors())
        .with_notice_targets(vec![connector_ref("discord-main", "general")]);
    assert_eq!(
        config.validate(),
        Err(RoutingConfigError::UnknownNoticeConnector(ConnectorId(
            "discord-main".into()
        )))
    );
}

#[test]
fn validate_accepts_the_test_fixture() {
    let config = RoutingConfig::new(vec![direct_rule(), mirror_rule()], connectors())
        .with_notice_targets(vec![connector_ref(TELEGRAM, "-100999")]);
    assert_eq!(config.validate(), Ok(()));
}

/// A routing source whose config the test can swap, as the settings store
/// does when a change is applied.
struct Swappable(Arc<std::sync::Mutex<Arc<RoutingConfig>>>);

impl Swappable {
    fn new(config: RoutingConfig) -> (Self, Arc<std::sync::Mutex<Arc<RoutingConfig>>>) {
        let shared = Arc::new(std::sync::Mutex::new(Arc::new(config)));
        (Self(Arc::clone(&shared)), shared)
    }
}

impl RoutingSource for Swappable {
    fn routing(&self) -> Arc<RoutingConfig> {
        Arc::clone(&self.0.lock().unwrap())
    }
}

/// Counts reads, to check each operation reads exactly once.
struct Counting {
    inner: Arc<RoutingConfig>,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl RoutingSource for Counting {
    fn routing(&self) -> Arc<RoutingConfig> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Arc::clone(&self.inner)
    }
}

fn swap(shared: &std::sync::Mutex<Arc<RoutingConfig>>, config: RoutingConfig) {
    *shared.lock().unwrap() = Arc::new(config);
}

#[tokio::test]
async fn rule_added_at_runtime_routes_the_next_event() {
    let (source, shared) = Swappable::new(RoutingConfig::new(vec![], connectors()));
    let core = Core::new(Arc::new(InMemoryStorage::default()), source);

    let before = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(before, IngestOutcome::Ignored(IgnoreReason::Unmatched));

    swap(
        &shared,
        RoutingConfig::new(vec![direct_rule()], connectors()),
    );
    let after = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-2", None))
        .await
        .unwrap();
    let dispatches = after.dispatches();
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(TELEGRAM.into()));
    assert_eq!(dispatches[0].to.address.chat_id, "-100123");
}

#[tokio::test]
async fn rule_removed_at_runtime_stops_the_conversation() {
    let (source, shared) = Swappable::new(RoutingConfig::new(vec![direct_rule()], connectors()));
    let core = Core::new(Arc::new(InMemoryStorage::default()), source);

    let routed = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(routed.dispatches().len(), 1);

    swap(&shared, RoutingConfig::new(vec![], connectors()));
    let after = core
        .ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-2", None))
        .await
        .unwrap();
    assert_eq!(after, IngestOutcome::Ignored(IgnoreReason::Unmatched));
}

#[tokio::test]
async fn notice_targets_follow_runtime_changes() {
    let with_target = |chat_id: &str| {
        RoutingConfig::new(vec![], connectors())
            .with_notice_targets(vec![connector_ref(TELEGRAM, chat_id)])
    };
    let (source, shared) = Swappable::new(with_target("-100111"));
    let core = Core::new(Arc::new(InMemoryStorage::default()), source);
    let whatsapp = ConnectorId(WHATSAPP.into());

    let first = core
        .notify_owners(whatsapp.clone(), NoticeKind::LoggedOut, "", &[], 1)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].to.address.chat_id, "-100111");

    swap(&shared, with_target("-100222"));
    let second = core
        .notify_owners(whatsapp, NoticeKind::Recovered, "", &[], 2)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].to.address.chat_id, "-100222");
}

#[tokio::test]
async fn each_operation_reads_routing_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let reads = Arc::new(AtomicUsize::new(0));
    let config = RoutingConfig::new(vec![mirror_rule()], connectors())
        .with_notice_targets(vec![connector_ref(TELEGRAM, "-100999")]);
    let core = Core::new(
        Arc::new(InMemoryStorage::default()),
        Counting {
            inner: Arc::new(config),
            reads: Arc::clone(&reads),
        },
    );
    let read_once = |before: usize| assert_eq!(reads.load(Ordering::SeqCst), before + 1);

    // A routed create: resolve, route, provision a thread, retitle check.
    let before = reads.load(Ordering::SeqCst);
    let routed = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
        .await
        .unwrap();
    read_once(before);

    // Acknowledging the provisioning dispatch replays the (empty) stash.
    let before = reads.load(Ordering::SeqCst);
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: routed.dispatches()[0].id,
        native_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();
    read_once(before);

    // A notice reads once for its targets and all of its sends.
    let before = reads.load(Ordering::SeqCst);
    core.notify_owners(
        ConnectorId(WHATSAPP.into()),
        NoticeKind::LoggedOut,
        "",
        &[],
        2,
    )
    .await
    .unwrap();
    read_once(before);

    // An owner message into the mirrored chat: resolve plus fan-out.
    let before = reads.load(Ordering::SeqCst);
    core.send_owner_message(OwnerMessage {
        connector_id: ConnectorId(WHATSAPP.into()),
        chat_id: "111@g.us".into(),
        thread_id: None,
        text: "hi".into(),
        media_ids: Vec::new(),
        client: None,
        reply_to_native_id: None,
        timestamp_ms: 3,
    })
    .await
    .unwrap();
    read_once(before);
}

// Chat profiles and disappearing timers (plan 045).

/// The test connectors, with WhatsApp declaring disappearing timers when
/// `capable`.
fn timer_connectors(capable: bool) -> Vec<Connector> {
    let mut connectors = connectors();
    connectors[0].capabilities.supports_disappearing_timers = capable;
    connectors
}

fn timer_core(rules: Vec<Rule>, capable: bool) -> Core<InMemoryStorage> {
    let config = RoutingConfig::new(rules, timer_connectors(capable));
    Core::new(Arc::new(InMemoryStorage::default()), Arc::new(config))
}

/// `event` carrying `timer` as its message's disappearing timer.
fn with_timer(mut event: Event, timer: DisappearingTimer) -> Event {
    let EventKind::NewMessage(message) = &mut event.payload else {
        panic!("only a new message carries a timer");
    };
    message.disappearing = Some(timer);
    event
}

async fn stored_timer(
    core: &Core<InMemoryStorage>,
    connector: &str,
    chat_id: &str,
) -> Option<DisappearingTimer> {
    core.storage()
        .chat_profile(&ConnectorId(connector.into()), chat_id)
        .await
        .unwrap()
        .and_then(|profile| profile.disappearing)
}

async fn report(core: &Core<InMemoryStorage>, chat_id: &str, timer: DisappearingTimer) {
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![ChatUpdate {
            chat_id: chat_id.into(),
            disappearing: Some(timer),
            ..ChatUpdate::default()
        }],
        false,
    )
    .await
    .unwrap();
}

fn sent_timer(dispatch: &Dispatch) -> Option<DisappearingTimer> {
    let Action::Send { disappearing, .. } = &dispatch.action else {
        panic!("expected a send, got {:?}", dispatch.action);
    };
    *disappearing
}

/// A Telegram reply in the direct rule's topic, routed back to WhatsApp.
async fn telegram_reply(core: &Core<InMemoryStorage>, native_id: &str) -> Dispatch {
    let outcome = core
        .ingest(event_in(TELEGRAM, "-100123", Some("12"), native_id, None))
        .await
        .unwrap();
    let dispatches = outcome.dispatches();
    assert_eq!(dispatches.len(), 1, "{outcome:?}");
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(WHATSAPP.into()));
    dispatches[0].clone()
}

#[test]
fn merge_disappearing_follows_the_setting_timestamp() {
    use crate::profile::merge_disappearing;
    let at = DisappearingTimer::set_at;
    let plain = DisappearingTimer::new;
    let cases = [
        // (stored, incoming, expected change)
        (None, plain(86_400), Some(plain(86_400))),
        (Some(at(86_400, 10)), at(604_800, 20), Some(at(604_800, 20))),
        (Some(at(86_400, 20)), at(604_800, 10), None),
        (Some(at(86_400, 20)), at(604_800, 20), Some(at(604_800, 20))),
        (Some(plain(86_400)), plain(604_800), Some(plain(604_800))),
        (Some(at(86_400, 20)), plain(86_400), None),
        (Some(at(86_400, 20)), plain(604_800), Some(plain(604_800))),
        (Some(plain(86_400)), at(86_400, 30), Some(at(86_400, 30))),
        (Some(plain(86_400)), plain(0), Some(plain(0))),
        (Some(at(86_400, 10)), at(0, 20), Some(at(0, 20))),
    ];
    for (stored, incoming, expected) in cases {
        assert_eq!(
            merge_disappearing(stored, incoming),
            expected,
            "stored {stored:?}, incoming {incoming:?}"
        );
    }
}

#[tokio::test]
async fn a_message_with_a_timer_records_the_chat_profile() {
    let core = core(vec![direct_rule()]);
    let outcome = core
        .ingest(with_timer(
            event(WHATSAPP, "99999@s.whatsapp.net", "wa-1", None),
            DisappearingTimer::new(86_400),
        ))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Ignored(IgnoreReason::Unmatched));
    assert_eq!(
        stored_timer(&core, WHATSAPP, "99999@s.whatsapp.net").await,
        Some(DisappearingTimer::new(86_400))
    );
}

#[tokio::test]
async fn chat_updates_apply_in_order() {
    let core = timer_core(vec![], true);
    report(
        &core,
        "1@s.whatsapp.net",
        DisappearingTimer::set_at(86_400, 5),
    )
    .await;

    let update = |chat_id: &str, timer| ChatUpdate {
        chat_id: chat_id.into(),
        disappearing: Some(timer),
        ..ChatUpdate::default()
    };
    let dispatches = core
        .update_chats(
            &ConnectorId(WHATSAPP.into()),
            vec![
                update("1@s.whatsapp.net", DisappearingTimer::set_at(86_400, 5)),
                update("2@s.whatsapp.net", DisappearingTimer::new(604_800)),
                update("3@g.us", DisappearingTimer::set_at(0, 7)),
                ChatUpdate {
                    chat_id: "4@g.us".into(),
                    disappearing: None,
                    ..ChatUpdate::default()
                },
            ],
            false,
        )
        .await
        .unwrap();
    // No chat is routed, so nothing is dispatched.
    assert!(dispatches.is_empty());
    assert_eq!(
        stored_timer(&core, WHATSAPP, "2@s.whatsapp.net").await,
        Some(DisappearingTimer::new(604_800))
    );
    assert_eq!(
        stored_timer(&core, WHATSAPP, "3@g.us").await,
        Some(DisappearingTimer::set_at(0, 7))
    );
    // An update that reports nothing writes nothing.
    assert_eq!(
        core.storage()
            .chat_profile(&ConnectorId(WHATSAPP.into()), "4@g.us")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_reply_into_a_chat_with_a_timer_carries_it() {
    let core = timer_core(vec![direct_rule()], true);
    let timer = DisappearingTimer::set_at(86_400, 1_700_000_000);
    let outcome = core
        .ingest(with_timer(
            event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None),
            timer,
        ))
        .await
        .unwrap();
    // Telegram does not stamp timers, so its copy carries none.
    assert_eq!(sent_timer(&outcome.dispatches()[0]), None);

    let reply = telegram_reply(&core, "tg-1").await;
    assert_eq!(sent_timer(&reply), Some(timer));
}

#[tokio::test]
async fn a_chat_known_to_be_off_carries_zero() {
    let core = timer_core(vec![direct_rule()], true);
    report(
        &core,
        "12345@s.whatsapp.net",
        DisappearingTimer::set_at(0, 9),
    )
    .await;

    let reply = telegram_reply(&core, "tg-1").await;
    assert_eq!(sent_timer(&reply), Some(DisappearingTimer::set_at(0, 9)));
}

#[tokio::test]
async fn a_target_without_the_capability_gets_no_timer() {
    let core = timer_core(vec![direct_rule()], false);
    core.ingest(with_timer(
        event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None),
        DisappearingTimer::new(86_400),
    ))
    .await
    .unwrap();
    // Recorded regardless, but never stamped on a send to this connector.
    assert_eq!(
        stored_timer(&core, WHATSAPP, "12345@s.whatsapp.net").await,
        Some(DisappearingTimer::new(86_400))
    );

    let reply = telegram_reply(&core, "tg-1").await;
    assert_eq!(sent_timer(&reply), None);
}

#[tokio::test]
async fn a_timer_learned_under_a_linked_form_is_found() {
    let lid_rule = Rule {
        flow: RuleKind::OneToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chat: ChatAddress::new("88887777@lid", None),
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress::new("-100123", Some("12".into())),
        },
        ..direct_rule()
    };
    let core = timer_core(vec![lid_rule], true);
    report(
        &core,
        "15550001111@s.whatsapp.net",
        DisappearingTimer::new(604_800),
    )
    .await;
    link_phone_to_lid(&core).await;

    let reply = telegram_reply(&core, "tg-1").await;
    assert_eq!(reply.to.address.chat_id, "88887777@lid");
    assert_eq!(sent_timer(&reply), Some(DisappearingTimer::new(604_800)));
}

#[tokio::test]
async fn owner_messages_carry_the_target_timer() {
    let core = timer_core(vec![direct_rule()], true);
    report(
        &core,
        "12345@s.whatsapp.net",
        DisappearingTimer::new(86_400),
    )
    .await;

    let outcome = core
        .send_owner_message(owner_message("12345@s.whatsapp.net", "hi", None))
        .await
        .unwrap();
    assert_eq!(
        sent_timer(&outcome.direct),
        Some(DisappearingTimer::new(86_400))
    );
    // The Telegram mirror of the owner's message carries none.
    assert_eq!(sent_timer(&outcome.mirrored[0]), None);
}

#[tokio::test]
async fn timer_notices_bypass_the_health_state() {
    let core = notifying_core();
    let whatsapp = ConnectorId(WHATSAPP.into());
    let notify = |kind| core.notify_owners(whatsapp.clone(), kind, "chat 1@s.whatsapp.net", &[], 1);

    assert_eq!(notify(NoticeKind::Degraded).await.unwrap().len(), 1);
    let first = notify(NoticeKind::DisappearingTimerUnknown).await.unwrap();
    assert_eq!(first.len(), 1);
    let text = notice_text(&first[0]);
    assert!(text.contains("will not disappear"), "{text}");
    assert!(text.contains("chat 1@s.whatsapp.net"), "{text}");
    assert_eq!(
        notify(NoticeKind::DisappearingTimerUnknown)
            .await
            .unwrap()
            .len(),
        1
    );
    // The timer notices left the health state alone: still a repeat.
    assert!(notify(NoticeKind::Degraded).await.unwrap().is_empty());

    // A recovery after only timer notices is not news.
    let fresh = notifying_core();
    fresh
        .notify_owners(
            whatsapp.clone(),
            NoticeKind::DisappearingTimerUnknown,
            "",
            &[],
            1,
        )
        .await
        .unwrap();
    assert!(
        fresh
            .notify_owners(whatsapp, NoticeKind::Recovered, "", &[], 2)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_media_notice_never_changes_health() {
    let core = notifying_core();
    let whatsapp = ConnectorId(WHATSAPP.into());
    let detail = "image from Alice in [P] Alice: download from WhatsApp: media expired";
    let notify = |kind| core.notify_owners(whatsapp.clone(), kind, detail, &[], 1);

    assert_eq!(notify(NoticeKind::Degraded).await.unwrap().len(), 1);
    // Every failed download is told, and none of them touches the health state.
    for _ in 0..2 {
        let sent = notify(NoticeKind::MediaUnavailable).await.unwrap();
        assert_eq!(sent.len(), 1);
        let text = notice_text(&sent[0]);
        assert!(text.contains("was not bridged"), "{text}");
        assert!(text.contains("media expired"), "{text}");
    }
    assert!(notify(NoticeKind::Degraded).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_message_without_a_timer_fills_an_unknown_chat_with_off() {
    let core = timer_core(vec![direct_rule()], true);
    core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(
        stored_timer(&core, WHATSAPP, "12345@s.whatsapp.net").await,
        Some(DisappearingTimer::new(0))
    );

    let reply = telegram_reply(&core, "tg-1").await;
    assert_eq!(sent_timer(&reply), Some(DisappearingTimer::new(0)));
}

#[tokio::test]
async fn a_message_without_a_timer_never_overrides_a_known_one() {
    for known in [
        DisappearingTimer::new(86_400),
        DisappearingTimer::set_at(86_400, 1_700_000_000),
    ] {
        let core = timer_core(vec![direct_rule()], true);
        report(&core, "12345@s.whatsapp.net", known).await;
        core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
            .await
            .unwrap();
        assert_eq!(
            stored_timer(&core, WHATSAPP, "12345@s.whatsapp.net").await,
            Some(known)
        );
    }
}

#[tokio::test]
async fn no_fill_from_a_connector_without_the_capability() {
    let core = timer_core(vec![direct_rule()], false);
    core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    assert_eq!(
        core.storage()
            .chat_profile(&ConnectorId(WHATSAPP.into()), "12345@s.whatsapp.net")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_reported_timer_replaces_the_fill() {
    let core = timer_core(vec![direct_rule()], true);
    core.ingest(event(WHATSAPP, "12345@s.whatsapp.net", "wa-1", None))
        .await
        .unwrap();
    core.ingest(with_timer(
        event(WHATSAPP, "12345@s.whatsapp.net", "wa-2", None),
        DisappearingTimer::new(604_800),
    ))
    .await
    .unwrap();
    assert_eq!(
        stored_timer(&core, WHATSAPP, "12345@s.whatsapp.net").await,
        Some(DisappearingTimer::new(604_800))
    );
}

// ---- Plan 046: chat profiles, notes and info cards ----

const GROUP: &str = "111@g.us";
const MIRROR_CHAT: &str = "-1001234567";

/// A WhatsApp group mirrored into a Telegram topic, with the group's timer
/// stamping on or off.
fn profile_core(capable: bool) -> Core<InMemoryStorage> {
    timer_core(vec![mirror_rule()], capable)
}

/// Route the group's first message and settle the provisioning: the info
/// card creates topic 777, the stashed message follows into it. Returns the
/// Telegram endpoint.
async fn provision_mirror(core: &Core<InMemoryStorage>) -> EndpointId {
    let card = core
        .ingest(event(WHATSAPP, GROUP, "wa-1", None))
        .await
        .unwrap()
        .dispatches()[0]
        .clone();
    assert!(matches!(card.action, Action::Note { .. }));
    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: card.id,
            native_id: "tg-card".into(),
            thread_id: Some("777".into()),
        })
        .await
        .unwrap();
    let message = released
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .expect("the stashed message follows the card")
        .clone();
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: message.id,
        native_id: "tg-1".into(),
        thread_id: None,
    })
    .await
    .unwrap();
    card.to.endpoint_id
}

async fn update_group(
    core: &Core<InMemoryStorage>,
    update: ChatUpdate,
    snapshot: bool,
) -> Vec<Dispatch> {
    core.update_chats(&ConnectorId(WHATSAPP.into()), vec![update], snapshot)
        .await
        .unwrap()
}

fn note_texts(dispatches: &[Dispatch]) -> Vec<String> {
    dispatches
        .iter()
        .filter_map(|dispatch| match &dispatch.action {
            Action::Note { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

async fn group_profile(core: &Core<InMemoryStorage>) -> wuphf_model::ChatProfile {
    core.storage()
        .chat_profile(&ConnectorId(WHATSAPP.into()), GROUP)
        .await
        .unwrap()
        .expect("the group has a profile")
}

/// Upload a picture the way a connector does, returning its attachment.
async fn upload_photo(core: &Core<InMemoryStorage>, bytes: &[u8]) -> Attachment {
    let attachment = Attachment {
        media_id: MediaId::new(),
        kind: AttachmentKind::Image,
        file_name: Some("avatar.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        size_bytes: Some(bytes.len() as u64),
        animated: false,
    };
    core.storage()
        .put_media(
            MediaBlob {
                attachment: attachment.clone(),
                bytes: bytes.to_vec(),
            },
            0,
        )
        .await
        .unwrap();
    attachment
}

fn avatar(id: &str, photo: Option<&Attachment>) -> ChatUpdate {
    ChatUpdate {
        avatar: Some(wuphf_model::ReportedAvatar {
            id: id.into(),
            media_id: photo.map(|photo| photo.media_id),
        }),
        ..ChatUpdate::new(GROUP)
    }
}

fn membership(
    kind: wuphf_model::ParticipantChangeKind,
    participant: &str,
) -> wuphf_model::ParticipantChange {
    wuphf_model::ParticipantChange {
        kind,
        participant: participant.into(),
        name: Some(participant.to_uppercase()),
        actor: None,
        actor_name: None,
    }
}

#[tokio::test]
async fn first_seen_values_are_a_silent_baseline() {
    let core = profile_core(false);
    provision_mirror(&core).await;
    let photo = upload_photo(&core, b"photo-a").await;

    let dispatches = update_group(
        &core,
        ChatUpdate {
            description: Some("Weekend plans".into()),
            participant_count: Some(10),
            disappearing: Some(DisappearingTimer::new(604_800)),
            ..avatar("pic-1", Some(&photo))
        },
        false,
    )
    .await;

    assert!(dispatches.is_empty(), "{dispatches:?}");
    let profile = group_profile(&core).await;
    assert_eq!(profile.description.as_deref(), Some("Weekend plans"));
    assert_eq!(profile.participant_count, Some(10));
    assert_eq!(profile.disappearing, Some(DisappearingTimer::new(604_800)));
    let stored = profile.avatar.expect("the avatar is stored");
    assert_eq!(stored.id, "pic-1");
    assert_eq!(stored.media_id, Some(photo.media_id));
    assert!(stored.hash.is_some());
}

#[tokio::test]
async fn a_description_change_posts_a_note_in_the_mirror_thread() {
    let core = profile_core(false);
    let telegram = provision_mirror(&core).await;
    let describe = |text: &str| ChatUpdate {
        description: Some(text.into()),
        ..ChatUpdate::new(GROUP)
    };
    update_group(&core, describe("Weekend plans"), false).await;

    let dispatches = update_group(&core, describe("New plans"), false).await;

    assert_eq!(dispatches.len(), 1, "only the mirror gets the note");
    let note = &dispatches[0];
    assert_eq!(note.to.endpoint_id, telegram);
    assert_eq!(
        note.to.address,
        ChatAddress::new(MIRROR_CHAT, Some("777".into()))
    );
    assert!(note.to.create_thread.is_none());
    assert_eq!(
        note_texts(&dispatches),
        ["📝 Description changed:\nNew plans"]
    );
}

#[tokio::test]
async fn a_snapshot_never_posts_notes() {
    let core = profile_core(false);
    provision_mirror(&core).await;
    let describe = |text: &str| ChatUpdate {
        description: Some(text.into()),
        ..ChatUpdate::new(GROUP)
    };
    update_group(&core, describe("Weekend plans"), false).await;

    let dispatches = update_group(&core, describe("Drifted while down"), true).await;

    assert!(dispatches.is_empty(), "{dispatches:?}");
    assert_eq!(
        group_profile(&core).await.description.as_deref(),
        Some("Drifted while down")
    );
}

#[tokio::test]
async fn the_same_photo_is_never_posted_twice() {
    let core = profile_core(false);
    provision_mirror(&core).await;
    let a = upload_photo(&core, b"photo-a").await;
    update_group(&core, avatar("pic-1", Some(&a)), false).await;

    // A new platform id, the same bytes: nothing changed.
    let same_bytes = upload_photo(&core, b"photo-a").await;
    let dispatches = update_group(&core, avatar("pic-2", Some(&same_bytes)), false).await;
    assert!(dispatches.is_empty(), "{dispatches:?}");
    let stored = group_profile(&core).await.avatar.unwrap();
    assert_eq!(stored.id, "pic-1");
    assert_eq!(stored.media_id, Some(a.media_id));

    // New bytes: one note, with the new photo attached.
    let b = upload_photo(&core, b"photo-b").await;
    let dispatches = update_group(&core, avatar("pic-3", Some(&b)), false).await;
    assert_eq!(note_texts(&dispatches), ["🖼️ Photo changed"]);
    let Action::Note { attachments, .. } = &dispatches[0].action else {
        unreachable!();
    };
    assert_eq!(attachments, &vec![b.clone()]);

    let dispatches = update_group(&core, avatar("", None), false).await;
    assert_eq!(note_texts(&dispatches), ["🖼️ Photo removed"]);
}

#[tokio::test]
async fn membership_changes_post_notes_and_adjust_the_count() {
    use wuphf_model::ParticipantChangeKind::{Joined, Left, Promoted};
    let core = profile_core(false);
    provision_mirror(&core).await;
    update_group(
        &core,
        ChatUpdate {
            participant_count: Some(10),
            ..ChatUpdate::new(GROUP)
        },
        false,
    )
    .await;

    let dispatches = update_group(
        &core,
        ChatUpdate {
            participant_changes: vec![
                membership(Joined, "b"),
                membership(Joined, "c"),
                membership(Left, "d"),
            ],
            ..ChatUpdate::new(GROUP)
        },
        false,
    )
    .await;
    assert_eq!(
        note_texts(&dispatches),
        ["➕ B joined", "➕ C joined", "➖ D left"]
    );
    assert_eq!(group_profile(&core).await.participant_count, Some(11));

    let dispatches = update_group(
        &core,
        ChatUpdate {
            participant_changes: vec![membership(Promoted, "b")],
            ..ChatUpdate::new(GROUP)
        },
        false,
    )
    .await;
    assert_eq!(note_texts(&dispatches), ["⭐ B is now an admin"]);
    assert_eq!(group_profile(&core).await.participant_count, Some(11));
}

#[tokio::test]
async fn a_title_change_renames_without_a_note() {
    let core = profile_core(false);
    provision_mirror(&core).await;
    let title = |title: &str| ChatUpdate {
        title: Some(title.into()),
        ..ChatUpdate::new(GROUP)
    };
    let renames = |dispatches: &[Dispatch]| -> Vec<String> {
        dispatches
            .iter()
            .filter_map(|dispatch| match &dispatch.action {
                Action::RenameThread { title } => Some(title.clone()),
                _ => None,
            })
            .collect()
    };

    let dispatches = update_group(&core, title("[G] New Name"), false).await;
    assert_eq!(renames(&dispatches), ["[G] New Name"]);
    assert!(note_texts(&dispatches).is_empty());

    let dispatches = update_group(&core, title("[G] Newer"), true).await;
    assert_eq!(
        renames(&dispatches),
        ["[G] Newer"],
        "a snapshot renames too"
    );
    assert!(note_texts(&dispatches).is_empty());
}

#[tokio::test]
async fn a_timer_change_posts_a_note() {
    let core = profile_core(true);
    provision_mirror(&core).await;
    // The first message, without a timer, settled the unknown timer as off.
    assert_eq!(
        stored_timer(&core, WHATSAPP, GROUP).await,
        Some(DisappearingTimer::new(0))
    );

    let dispatches = update_group(
        &core,
        ChatUpdate {
            disappearing: Some(DisappearingTimer::new(604_800)),
            ..ChatUpdate::new(GROUP)
        },
        false,
    )
    .await;
    assert_eq!(
        note_texts(&dispatches),
        ["⏳ Disappearing messages: 7 days"]
    );

    // A message carrying a different timer notes it ahead of the message.
    let outcome = core
        .ingest(with_timer(
            event(WHATSAPP, GROUP, "wa-2", None),
            DisappearingTimer::new(86_400),
        ))
        .await
        .unwrap();
    let dispatches = outcome.dispatches();
    assert_eq!(
        note_texts(dispatches),
        ["⏳ Disappearing messages: 24 hours"]
    );
    assert!(matches!(dispatches[0].action, Action::Note { .. }));
    assert!(matches!(dispatches[1].action, Action::Send { .. }));

    // The unknown fill (a first-seen value) posts nothing.
    core.storage()
        .put_chat_profile(&wuphf_model::ChatProfile::new(
            ConnectorId(WHATSAPP.into()),
            GROUP,
        ))
        .await
        .unwrap();
    let outcome = core
        .ingest(event(WHATSAPP, GROUP, "wa-3", None))
        .await
        .unwrap();
    assert!(note_texts(outcome.dispatches()).is_empty());
    assert_eq!(
        stored_timer(&core, WHATSAPP, GROUP).await,
        Some(DisappearingTimer::new(0))
    );
}

#[tokio::test]
async fn notes_skip_unprovisioned_mirrors() {
    let core = profile_core(false);
    // The card is out, the topic does not exist yet.
    core.ingest(event(WHATSAPP, GROUP, "wa-1", None))
        .await
        .unwrap();
    let describe = |text: &str| ChatUpdate {
        description: Some(text.into()),
        ..ChatUpdate::new(GROUP)
    };
    update_group(&core, describe("Weekend plans"), false).await;

    let dispatches = update_group(&core, describe("New plans"), false).await;

    assert!(dispatches.is_empty(), "{dispatches:?}");
}

#[tokio::test]
async fn a_new_mirror_topic_starts_with_an_info_card() {
    let core = profile_core(false);
    let photo = upload_photo(&core, b"photo-a").await;
    update_group(
        &core,
        ChatUpdate {
            title: Some("[G] Family".into()),
            description: Some("Weekend plans".into()),
            participant_count: Some(12),
            disappearing: Some(DisappearingTimer::new(604_800)),
            ..avatar("pic-1", Some(&photo))
        },
        true,
    )
    .await;

    let outcome = core
        .ingest(event(WHATSAPP, GROUP, "wa-1", None))
        .await
        .unwrap();
    let [card] = outcome.dispatches() else {
        panic!("the card is the only dispatch: {outcome:?}");
    };
    assert!(card.to.create_thread.is_some());
    let Action::Note { text, attachments } = &card.action else {
        panic!("the card is a note: {card:?}");
    };
    assert_eq!(
        text,
        "ℹ️ Title\n🔌 whatsapp-main\n📝 Weekend plans\n⏳ Disappearing messages: 7 days\n👥 12 \
         participants"
    );
    assert_eq!(attachments, &vec![photo]);

    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: card.id,
            native_id: "tg-card".into(),
            thread_id: Some("777".into()),
        })
        .await
        .unwrap();
    let message = released
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .expect("the message follows the card");
    assert!(message.to.create_thread.is_none());
    assert_eq!(message.to.address.thread_id.as_deref(), Some("777"));
}

#[tokio::test]
async fn one_card_per_conversation() {
    let core = profile_core(false);
    let card = core
        .ingest(event(WHATSAPP, GROUP, "wa-1", None))
        .await
        .unwrap()
        .dispatches()[0]
        .clone();
    assert!(matches!(card.action, Action::Note { .. }));

    // The card fails: the stashed message provisions the topic itself.
    let released = core
        .fail_dispatch(card.id, "cannot create topic".into())
        .await
        .unwrap();
    let [message] = released.as_slice() else {
        panic!("only the message is released: {released:?}");
    };
    assert!(matches!(message.action, Action::Send { .. }));
    assert!(message.to.create_thread.is_some());
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: message.id,
        native_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();

    // The thread is lost and provisioned again: still no second card.
    let mut endpoint = core
        .storage()
        .endpoint(message.to.endpoint_id)
        .await
        .unwrap();
    endpoint.thread_id = None;
    core.storage().put_endpoint(endpoint).await.unwrap();
    let outcome = core
        .ingest(event(WHATSAPP, GROUP, "wa-2", None))
        .await
        .unwrap();
    let [again] = outcome.dispatches() else {
        panic!("one dispatch: {outcome:?}");
    };
    assert!(matches!(again.action, Action::Send { .. }));
    assert!(again.to.create_thread.is_some());
}

#[tokio::test]
async fn a_note_records_no_copy() {
    let core = profile_core(false);
    let telegram = provision_mirror(&core).await;
    let describe = |text: &str| ChatUpdate {
        description: Some(text.into()),
        ..ChatUpdate::new(GROUP)
    };
    update_group(&core, describe("Weekend plans"), false).await;
    let note = update_group(&core, describe("New plans"), false).await[0].clone();

    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: note.id,
            native_id: "tg-note".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    assert!(released.is_empty(), "no delivery ack: {released:?}");
    assert_eq!(
        core.storage()
            .message_id_for_copy(telegram, "tg-note")
            .await
            .unwrap(),
        None
    );

    // A Telegram reply quoting the note bridges with no reply link.
    let outcome = core
        .ingest(event_in(
            TELEGRAM,
            MIRROR_CHAT,
            Some("777"),
            "tg-reply",
            Some("tg-note"),
        ))
        .await
        .unwrap();
    let [reply] = outcome.dispatches() else {
        panic!("one dispatch: {outcome:?}");
    };
    let Action::Send { message, reply, .. } = &reply.action else {
        panic!("a send: {reply:?}");
    };
    assert!(reply.is_none());
    assert!(message.reply_to.is_none());
}

#[tokio::test]
async fn owner_message_provisioning_also_gets_a_card() {
    let core = profile_core(false);
    let outcome = core
        .send_owner_message(owner_message(GROUP, "from the tui", None))
        .await
        .unwrap();
    assert!(matches!(outcome.direct.action, Action::Send { .. }));
    let [card] = outcome.mirrored.as_slice() else {
        panic!("the card is the only mirrored dispatch: {outcome:?}");
    };
    assert!(matches!(card.action, Action::Note { .. }));
    assert!(card.to.create_thread.is_some());
}

#[tokio::test]
async fn one_change_reported_under_two_forms_posts_one_note() {
    let core = profile_core(false);
    provision_mirror(&core).await;
    // The platform reports a second address form for the group.
    core.migrate_identity(
        ConnectorId(WHATSAPP.into()),
        ChatAddress::new(GROUP, None),
        ChatAddress::new("222@g.us", None),
    )
    .await
    .unwrap();
    let describe = |chat: &str, text: &str| ChatUpdate {
        description: Some(text.into()),
        ..ChatUpdate::new(chat)
    };
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![
            describe(GROUP, "Weekend plans"),
            describe("222@g.us", "Weekend plans"),
        ],
        false,
    )
    .await
    .unwrap();

    let dispatches = core
        .update_chats(
            &ConnectorId(WHATSAPP.into()),
            vec![
                describe(GROUP, "New plans"),
                describe("222@g.us", "New plans"),
            ],
            false,
        )
        .await
        .unwrap();

    assert_eq!(
        note_texts(&dispatches),
        ["📝 Description changed:\nNew plans"]
    );
    assert_eq!(
        core.pending_dispatches()
            .await
            .unwrap()
            .iter()
            .filter(|dispatch| matches!(dispatch.action, Action::Note { .. }))
            .count(),
        1,
        "no duplicate waits in storage either"
    );
}

// ---- Plan 047: two-way pin, mute and archive ----

const OTHER_GROUP: &str = "222@g.us";

/// A many-to-one rule mirroring only `group` into the Telegram forum.
fn group_rule(id: &str, group: &str) -> Rule {
    Rule {
        id: RuleId(id.into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chats_filter: ChatFilter::Only {
                chats: vec![ChatAddress::new(group, None)],
            },
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress::new(MIRROR_CHAT, None),
        },
    }
}

fn state_core(settings: ChatStateSettings) -> Core<InMemoryStorage> {
    let config = RoutingConfig::new(
        vec![group_rule("family", GROUP), group_rule("work", OTHER_GROUP)],
        connectors(),
    )
    .with_chat_state(settings);
    Core::new(Arc::new(InMemoryStorage::default()), Arc::new(config))
}

/// Settle `group`'s first message into topic `thread`; returns the
/// conversation's WhatsApp and Telegram endpoints.
async fn provision_group(
    core: &Core<InMemoryStorage>,
    group: &str,
    thread: &str,
) -> (EndpointId, EndpointId) {
    let card = core
        .ingest(event(WHATSAPP, group, &format!("{group}-1"), None))
        .await
        .unwrap()
        .dispatches()[0]
        .clone();
    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: card.id,
            native_id: format!("{group}-card"),
            thread_id: Some(thread.into()),
        })
        .await
        .unwrap();
    let message = released
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .unwrap()
        .clone();
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: message.id,
        native_id: format!("{group}-tg-1"),
        thread_id: None,
    })
    .await
    .unwrap();
    let conversation = core
        .storage()
        .conversation(card.conversation_id)
        .await
        .unwrap()
        .unwrap();
    let whatsapp = conversation
        .endpoints
        .iter()
        .copied()
        .find(|endpoint| *endpoint != card.to.endpoint_id)
        .unwrap();
    (whatsapp, card.to.endpoint_id)
}

fn pin(pinned: bool) -> ChatUpdate {
    ChatUpdate {
        pinned: Some(pinned),
        ..ChatUpdate::default()
    }
}

fn mute(muted_until: i64) -> ChatUpdate {
    ChatUpdate {
        mute: Some(wuphf_model::MuteState { muted_until }),
        ..ChatUpdate::default()
    }
}

fn archive(archived: bool) -> ChatUpdate {
    ChatUpdate {
        archived: Some(archived),
        ..ChatUpdate::default()
    }
}

/// `state` reported by WhatsApp about `group`.
async fn from_whatsapp(
    core: &Core<InMemoryStorage>,
    group: &str,
    state: ChatUpdate,
    snapshot: bool,
) -> Vec<Dispatch> {
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![ChatUpdate {
            chat_id: group.into(),
            ..state
        }],
        snapshot,
    )
    .await
    .unwrap()
}

/// `state` reported by Telegram about topic `thread`.
async fn from_topic(
    core: &Core<InMemoryStorage>,
    thread: &str,
    state: ChatUpdate,
) -> Vec<Dispatch> {
    core.update_chats(
        &ConnectorId(TELEGRAM.into()),
        vec![ChatUpdate {
            chat_id: MIRROR_CHAT.into(),
            thread_id: Some(thread.into()),
            ..state
        }],
        false,
    )
    .await
    .unwrap()
}

/// A chat-state dispatch's endpoint, pinned, mute and archived.
type SetState = (
    EndpointId,
    Option<bool>,
    Option<wuphf_model::MuteState>,
    Option<bool>,
);

/// Each chat-state dispatch: its endpoint and the fields it carries.
fn set_states(dispatches: &[Dispatch]) -> Vec<SetState> {
    dispatches
        .iter()
        .filter_map(|dispatch| match &dispatch.action {
            Action::SetChatState {
                pinned,
                mute,
                archived,
            } => Some((dispatch.to.endpoint_id, *pinned, *mute, *archived)),
            _ => None,
        })
        .collect()
}

async fn stored_state(core: &Core<InMemoryStorage>, group: &str) -> wuphf_model::ChatProfile {
    core.storage()
        .chat_profile(&ConnectorId(WHATSAPP.into()), group)
        .await
        .unwrap()
        .unwrap_or_else(|| wuphf_model::ChatProfile::new(ConnectorId(WHATSAPP.into()), group))
}

fn policy(
    pin: wuphf_model::ChatStateDirection,
    mute: wuphf_model::ChatStateDirection,
    archive: wuphf_model::ChatStateDirection,
) -> ChatStateSettings {
    ChatStateSettings {
        global: wuphf_model::ChatStatePolicy { pin, mute, archive },
        overrides: Vec::new(),
    }
}

#[tokio::test]
async fn a_whatsapp_pin_pins_the_topic() {
    let core = state_core(ChatStateSettings::default());
    let (_, telegram) = provision_group(&core, GROUP, "777").await;

    let dispatches = from_whatsapp(&core, GROUP, pin(true), false).await;

    assert_eq!(
        set_states(&dispatches),
        [(telegram, Some(true), None, None)]
    );
    assert_eq!(
        dispatches[0].to.address,
        ChatAddress::new(MIRROR_CHAT, Some("777".into()))
    );
}

#[tokio::test]
async fn the_topic_pin_echo_is_a_no_op() {
    let core = state_core(ChatStateSettings::default());
    provision_group(&core, GROUP, "777").await;
    from_whatsapp(&core, GROUP, pin(true), false).await;
    let before = stored_state(&core, GROUP).await;

    let dispatches = from_topic(&core, "777", pin(true)).await;

    assert!(dispatches.is_empty(), "{dispatches:?}");
    assert_eq!(stored_state(&core, GROUP).await, before);
}

#[tokio::test]
async fn a_topic_close_archives_the_whatsapp_chat() {
    let core = state_core(ChatStateSettings::default());
    let (whatsapp, _) = provision_group(&core, GROUP, "777").await;

    let dispatches = from_topic(&core, "777", archive(true)).await;
    assert_eq!(
        set_states(&dispatches),
        [(whatsapp, None, None, Some(true))]
    );
    assert_eq!(dispatches[0].to.address, ChatAddress::new(GROUP, None));

    let echo = from_whatsapp(&core, GROUP, archive(true), false).await;
    assert!(echo.is_empty(), "{echo:?}");
}

#[tokio::test]
async fn mute_values_cross_exactly() {
    let core = state_core(ChatStateSettings::default());
    let (whatsapp, telegram) = provision_group(&core, GROUP, "777").await;
    let until = |muted_until| Some(wuphf_model::MuteState { muted_until });

    let dispatches = from_whatsapp(&core, GROUP, mute(1_900_000_000), false).await;
    assert_eq!(
        set_states(&dispatches),
        [(telegram, None, until(1_900_000_000), None)]
    );
    assert!(
        from_topic(&core, "777", mute(1_900_000_000))
            .await
            .is_empty()
    );

    let forever = wuphf_model::MuteState::FOREVER;
    let dispatches = from_topic(&core, "777", mute(forever)).await;
    assert_eq!(
        set_states(&dispatches),
        [(whatsapp, None, until(forever), None)]
    );
    assert!(
        from_whatsapp(&core, GROUP, mute(forever), false)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn directions_gate_each_field() {
    use wuphf_model::ChatStateDirection::{Off, ToMirror, ToSource};
    let core = state_core(policy(ToSource, Off, ToMirror));
    let (whatsapp, _) = provision_group(&core, GROUP, "777").await;

    // pin = to_source: a WhatsApp pin is stored but not dispatched, so the
    // topic reporting the same pin is no change; an unpin there crosses.
    assert!(
        from_whatsapp(&core, GROUP, pin(true), false)
            .await
            .is_empty()
    );
    assert_eq!(stored_state(&core, GROUP).await.pinned, Some(true));
    assert!(from_topic(&core, "777", pin(true)).await.is_empty());
    let dispatches = from_topic(&core, "777", pin(false)).await;
    assert_eq!(
        set_states(&dispatches),
        [(whatsapp, Some(false), None, None)]
    );
    assert_eq!(stored_state(&core, GROUP).await.pinned, Some(false));

    // mute = off: neither side's mute moves; the source's is still recorded.
    assert!(
        from_whatsapp(&core, GROUP, mute(-1), false)
            .await
            .is_empty()
    );
    assert!(from_topic(&core, "777", mute(0)).await.is_empty());
    assert_eq!(
        stored_state(&core, GROUP).await.mute,
        Some(wuphf_model::MuteState { muted_until: -1 })
    );

    // archive = to_mirror: a topic close is dropped, the stored value stays.
    assert!(from_topic(&core, "777", archive(true)).await.is_empty());
    assert_eq!(stored_state(&core, GROUP).await.archived, None);
}

#[tokio::test]
async fn an_override_beats_the_global_setting() {
    let family = "whatsapp-main:111@g.us".parse().unwrap();
    let core = state_core(ChatStateSettings {
        global: wuphf_model::ChatStatePolicy::default(),
        overrides: vec![(
            family,
            wuphf_model::ChatStatePolicyOverride {
                pin: Some(wuphf_model::ChatStateDirection::Off),
                ..Default::default()
            },
        )],
    });
    provision_group(&core, GROUP, "777").await;
    let (_, work_topic) = provision_group(&core, OTHER_GROUP, "778").await;

    assert!(
        from_whatsapp(&core, GROUP, pin(true), false)
            .await
            .is_empty()
    );
    let dispatches = from_whatsapp(&core, OTHER_GROUP, pin(true), false).await;
    assert_eq!(
        set_states(&dispatches),
        [(work_topic, Some(true), None, None)]
    );
}

#[tokio::test]
async fn a_snapshot_sets_the_baseline_silently() {
    let core = state_core(ChatStateSettings::default());
    let (_, telegram) = provision_group(&core, GROUP, "777").await;

    assert!(
        from_whatsapp(&core, GROUP, pin(true), true)
            .await
            .is_empty()
    );
    assert_eq!(stored_state(&core, GROUP).await.pinned, Some(true));

    let dispatches = from_whatsapp(&core, GROUP, pin(false), false).await;
    assert_eq!(
        set_states(&dispatches),
        [(telegram, Some(false), None, None)]
    );
}

#[tokio::test]
async fn no_topic_no_dispatch() {
    let core = state_core(ChatStateSettings::default());
    // The card is out; the topic does not exist yet.
    core.ingest(event(WHATSAPP, GROUP, "wa-1", None))
        .await
        .unwrap();

    assert!(
        from_whatsapp(&core, GROUP, pin(true), false)
            .await
            .is_empty()
    );
    assert_eq!(stored_state(&core, GROUP).await.pinned, Some(true));
}

#[tokio::test]
async fn the_archive_unpin_crosses_too() {
    let core = state_core(ChatStateSettings::default());
    let (_, telegram) = provision_group(&core, GROUP, "777").await;
    from_whatsapp(&core, GROUP, pin(true), false).await;

    // WhatsApp archives, which also unpins.
    let mut dispatches = from_whatsapp(&core, GROUP, archive(true), false).await;
    dispatches.extend(from_whatsapp(&core, GROUP, pin(false), false).await);

    assert_eq!(
        set_states(&dispatches),
        [
            (telegram, None, None, Some(true)),
            (telegram, Some(false), None, None),
        ]
    );
}

#[tokio::test]
async fn a_migrated_chat_s_profile_is_read_from_its_new_form_first() {
    let core = profile_core(false);
    let whatsapp = ConnectorId(WHATSAPP.into());
    core.ingest(event(WHATSAPP, GROUP, "wa-1", None))
        .await
        .unwrap();
    update_group(
        &core,
        ChatUpdate {
            title: Some("[G] Old".into()),
            disappearing: Some(DisappearingTimer::new(604_800)),
            ..ChatUpdate::new(GROUP)
        },
        true,
    )
    .await;
    // The platform now names the chat by another form, and reports there.
    core.migrate_identity(
        whatsapp.clone(),
        ChatAddress::new(GROUP, None),
        ChatAddress::new("222@g.us", None),
    )
    .await
    .unwrap();
    core.update_chats(
        &whatsapp,
        vec![ChatUpdate {
            title: Some("[G] New".into()),
            ..ChatUpdate::new("222@g.us")
        }],
        false,
    )
    .await
    .unwrap();

    let [(conversation, _)] = core
        .conversations_of_chat(&whatsapp, GROUP)
        .await
        .unwrap()
        .try_into()
        .expect("one conversation");
    let profile = core.source_profile(&conversation).await.unwrap().unwrap();
    // The current form's title wins over the conversation's own stale one; the
    // timer known only under the old form fills the gap.
    assert_eq!(profile.title.as_deref(), Some("[G] New"));
    assert_eq!(profile.disappearing, Some(DisappearingTimer::new(604_800)));
}

// ---- health pauses (plan 079) ----

fn forward_pause() -> HealthPause {
    HealthPause {
        rule: RuleId("wa-mirror".into()),
        direction: wuphf_model::FlowDirection::Forward,
        connector: ConnectorId(TELEGRAM.into()),
        chat_id: "-1001234567".into(),
    }
}

fn back_pause() -> HealthPause {
    HealthPause {
        rule: RuleId("wa-mirror".into()),
        direction: wuphf_model::FlowDirection::Back,
        connector: ConnectorId(WHATSAPP.into()),
        chat_id: "111@g.us".into(),
    }
}

fn routed(outcome: IngestOutcome) -> (Vec<Dispatch>, usize) {
    match outcome {
        IngestOutcome::Routed {
            dispatches,
            blocked,
            ..
        } => (dispatches, blocked),
        other => panic!("expected a routed outcome, got {other:?}"),
    }
}

#[tokio::test]
async fn a_paused_target_holds_the_thread_creation_and_stashes_the_rest() {
    let storage = Arc::new(InMemoryStorage::default());
    let core = core_over(Arc::clone(&storage), vec![mirror_rule()]);
    core.set_health_pauses(BTreeSet::from([forward_pause()]));

    // The first message would create the topic (its info card): held, not sent.
    let (dispatches, blocked) = routed(
        core.ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
            .await
            .unwrap(),
    );
    assert!(dispatches.is_empty());
    assert_eq!(blocked, 1);
    let held = storage.held_dispatches().await.unwrap();
    assert_eq!(held.len(), 1);
    assert!(held[0].dispatch.to.create_thread.is_some());
    assert!(storage.pending_dispatches().await.unwrap().is_empty());

    // A second message waits for that topic, as while it is being created,
    // instead of creating one of its own.
    assert!(matches!(
        core.ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
            .await
            .unwrap(),
        IngestOutcome::Deferred { .. }
    ));

    // Releasing sends the topic creation; its acknowledgement replays the
    // stashed message into the new topic.
    core.set_health_pauses(BTreeSet::new());
    let released = core.release_held(&forward_pause(), 0).await.unwrap();
    assert_eq!(released.dropped, 0);
    assert_eq!(released.dispatches.len(), 1);
    let card = &released.dispatches[0];
    assert_eq!(
        &storage.pending_dispatches().await.unwrap(),
        std::slice::from_ref(card)
    );
    let replayed = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: card.id,
            native_id: "tg-card".into(),
            thread_id: Some("777".into()),
        })
        .await
        .unwrap();
    assert!(replayed.iter().any(|dispatch| {
        matches!(dispatch.action, Action::Send { .. })
            && dispatch.to.address.thread_id.as_deref() == Some("777")
    }));
}

#[tokio::test]
async fn a_held_thread_creation_survives_a_restart() {
    let storage = Arc::new(InMemoryStorage::default());
    let core = core_over(Arc::clone(&storage), vec![mirror_rule()]);
    core.set_health_pauses(BTreeSet::from([forward_pause()]));
    routed(
        core.ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
            .await
            .unwrap(),
    );

    // A new core recovers the held creation as the conversation's
    // provisioning, so its stash is not replayed as if it had settled.
    let restarted = core_over(Arc::clone(&storage), vec![mirror_rule()]);
    restarted.set_health_pauses(BTreeSet::from([forward_pause()]));
    assert!(restarted.recover_provisioning().await.unwrap().is_empty());
    assert!(matches!(
        restarted
            .ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
            .await
            .unwrap(),
        IngestOutcome::Deferred { .. }
    ));
}

#[tokio::test]
async fn release_drops_what_was_held_before_the_cutoff() {
    let storage = Arc::new(InMemoryStorage::default());
    let core = core_over(Arc::clone(&storage), vec![mirror_rule()]);
    core.set_health_pauses(BTreeSet::from([forward_pause()]));
    routed(
        core.ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
            .await
            .unwrap(),
    );
    core.ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
        .await
        .unwrap();

    // Everything is older than a cutoff in the future: both messages, stashed
    // behind the topic's info card, are dropped; the card that creates the
    // topic is kept (later messages need the topic).
    let released = core.release_held(&forward_pause(), i64::MAX).await.unwrap();
    assert_eq!(released.dropped, 2);
    assert_eq!(released.dispatches.len(), 1);
    assert!(released.dispatches[0].to.create_thread.is_some());
}

#[tokio::test]
async fn a_back_pause_holds_replies_but_not_the_mirror() {
    let core = core(vec![mirror_rule()]);
    // Provision the topic first.
    let (dispatches, _) = routed(
        core.ingest(event(WHATSAPP, "111@g.us", "wa-1", None))
            .await
            .unwrap(),
    );
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: dispatches[0].id,
        native_id: "tg-card".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();
    core.set_health_pauses(BTreeSet::from([back_pause()]));

    // A reply typed in the topic goes back to WhatsApp: held.
    let (dispatches, blocked) = routed(
        core.ingest(event_in(TELEGRAM, "-1001234567", Some("777"), "tg-9", None))
            .await
            .unwrap(),
    );
    assert!(dispatches.is_empty());
    assert_eq!(blocked, 1);
    // A new WhatsApp message still mirrors into the topic.
    let (dispatches, _) = routed(
        core.ingest(event(WHATSAPP, "111@g.us", "wa-3", None))
            .await
            .unwrap(),
    );
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(TELEGRAM.into()));
}

#[tokio::test]
async fn an_owner_message_outside_the_rules_is_never_held() {
    let core = core(vec![mirror_rule()]);
    core.set_health_pauses(BTreeSet::from([HealthPause {
        rule: RuleId("wa-mirror".into()),
        direction: wuphf_model::FlowDirection::Forward,
        connector: ConnectorId(WHATSAPP.into()),
        chat_id: "999@g.us".into(),
    }]));
    let outcome = core
        .send_owner_message(owner_message("999@g.us", "hi", None))
        .await
        .unwrap();
    assert_eq!(outcome.direct.to.address.chat_id, "999@g.us");
}

// Plan 080: the owner's reactions.

const OWNER: &str = "owner-1";

/// A core whose Telegram connector names `OWNER` among its owner ids.
fn core_with_owner(rules: Vec<Rule>) -> Core<InMemoryStorage> {
    let owners = HashMap::from([(
        ConnectorId(TELEGRAM.into()),
        HashSet::from([OWNER.to_owned()]),
    )]);
    let config = Arc::new(RoutingConfig::new(rules, connectors()).with_owners(owners));
    Core::new(Arc::new(InMemoryStorage::default()), config)
}

/// A reaction in the direct rule's Telegram topic.
fn telegram_reaction(native_id: &str, reactor: &str, emoji: Option<&str>) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(TELEGRAM.into()),
            address: ChatAddress::new("-100123", Some("12".into())),
            chat_title: None,
        },
        payload: EventKind::Reaction {
            native_id: native_id.into(),
            reactor: reactor.into(),
            reactor_name: Some("Owner".into()),
            emoji: emoji.map(str::to_owned),
            from_me: false,
        },
    }
}

/// The complete set of reactions on a message in the Telegram topic.
fn telegram_reaction_set(native_id: &str, reactions: &[(&str, &str)]) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(TELEGRAM.into()),
            address: ChatAddress::new("-100123", Some("12".into())),
            chat_title: None,
        },
        payload: EventKind::ReactionSet {
            native_id: native_id.into(),
            reactions: reactions
                .iter()
                .map(|(reactor, emoji)| ReactorReaction {
                    reactor: (*reactor).into(),
                    reactor_name: None,
                    emoji: (*emoji).into(),
                })
                .collect(),
        },
    }
}

/// The paired WhatsApp account's own reaction (`from_me`) on `wa-1`.
fn own_whatsapp_reaction(emoji: Option<&str>) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(WHATSAPP.into()),
            address: ChatAddress::new("12345@s.whatsapp.net", None),
            chat_title: None,
        },
        payload: EventKind::Reaction {
            native_id: "wa-1".into(),
            reactor: "me@s.whatsapp.net".into(),
            reactor_name: None,
            emoji: emoji.map(str::to_owned),
            from_me: true,
        },
    }
}

/// A WhatsApp message `wa-1` mirrored to Telegram as `tg-1`.
async fn mirrored_whatsapp_message(core: &Core<InMemoryStorage>) {
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
    core.record_dispatched_copy(message_id, dispatches[0].to.endpoint_id, "tg-1")
        .await
        .unwrap();
}

/// The one reaction dispatch an outcome carries.
fn only_reaction(outcome: &IngestOutcome) -> (&Dispatch, bool, &MessageAuthor, Option<&str>) {
    let [dispatch] = outcome.dispatches() else {
        panic!("one dispatch expected: {outcome:?}");
    };
    let Action::React {
        by_owner,
        target_author,
        emoji,
        ..
    } = &dispatch.action
    else {
        panic!("a reaction expected: {dispatch:?}");
    };
    (dispatch, *by_owner, target_author, emoji.as_deref())
}

#[tokio::test]
async fn an_owners_telegram_reaction_reaches_whatsapp_as_the_owners() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;

    let outcome = core
        .ingest(telegram_reaction("tg-1", OWNER, Some("❤️")))
        .await
        .unwrap();
    let (dispatch, by_owner, author, emoji) = only_reaction(&outcome);
    assert_eq!(dispatch.to.connector_id, ConnectorId(WHATSAPP.into()));
    assert!(by_owner);
    assert_eq!(emoji, Some("❤️"));
    // The WhatsApp copy is the one WhatsApp delivered: its author is alice.
    assert_eq!(
        *author,
        MessageAuthor {
            author_id: "alice".into(),
            participant: None,
            authored_by_bridge: false,
        }
    );
    let Action::React { native_id, .. } = &dispatch.action else {
        unreachable!();
    };
    assert_eq!(native_id, "wa-1");
}

#[tokio::test]
async fn someone_elses_reaction_is_not_the_owners() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;

    let outcome = core
        .ingest(telegram_reaction("tg-1", "stranger", Some("😂")))
        .await
        .unwrap();
    let (_, by_owner, _, _) = only_reaction(&outcome);
    assert!(!by_owner);
}

#[tokio::test]
async fn a_reaction_on_a_copy_the_bridge_sent_names_the_bridge_as_its_author() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    let IngestOutcome::Routed {
        message_id,
        dispatches,
        ..
    } = core
        .ingest(event_in(TELEGRAM, "-100123", Some("12"), "tg-5", None))
        .await
        .unwrap()
    else {
        panic!("a reply typed in the topic should route");
    };
    core.record_dispatched_copy(message_id, dispatches[0].to.endpoint_id, "wa-5")
        .await
        .unwrap();

    let outcome = core
        .ingest(telegram_reaction("tg-5", OWNER, Some("👍")))
        .await
        .unwrap();
    let (_, _, author, _) = only_reaction(&outcome);
    assert!(author.authored_by_bridge);
}

#[tokio::test]
async fn a_blocked_owner_reaction_gets_its_author_on_release() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    let IngestOutcome::Routed { dispatches, .. } = core
        .ingest(event_in(TELEGRAM, "-100123", Some("12"), "tg-5", None))
        .await
        .unwrap()
    else {
        panic!("a reply typed in the topic should route");
    };
    let create = dispatches[0].clone();

    let outcome = core
        .ingest(telegram_reaction("tg-5", OWNER, Some("👍")))
        .await
        .unwrap();
    assert!(matches!(outcome, IngestOutcome::Routed { blocked: 1, .. }));

    let released = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: create.id,
            native_id: "wa-5".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    let reaction = released
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::React { .. }))
        .expect("acknowledging the create releases the reaction");
    let Action::React {
        native_id,
        by_owner,
        target_author,
        ..
    } = &reaction.action
    else {
        unreachable!();
    };
    assert_eq!(native_id, "wa-5");
    assert!(by_owner);
    assert!(target_author.authored_by_bridge);
}

#[tokio::test]
async fn a_repeated_reaction_is_ignored() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    core.ingest(telegram_reaction("tg-1", OWNER, Some("❤️")))
        .await
        .unwrap();

    let again = core
        .ingest(telegram_reaction("tg-1", OWNER, Some("❤️")))
        .await
        .unwrap();
    assert_eq!(
        again,
        IngestOutcome::Ignored(IgnoreReason::RedundantReaction)
    );
}

#[tokio::test]
async fn a_reaction_set_is_diffed_with_the_stored_reactions() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;

    let set = core
        .ingest(telegram_reaction_set("tg-1", &[(OWNER, "❤️")]))
        .await
        .unwrap();
    let (_, by_owner, _, emoji) = only_reaction(&set);
    assert!(by_owner);
    assert_eq!(emoji, Some("❤️"));

    let unchanged = core
        .ingest(telegram_reaction_set("tg-1", &[(OWNER, "❤️")]))
        .await
        .unwrap();
    assert_eq!(
        unchanged,
        IngestOutcome::Ignored(IgnoreReason::RedundantReaction)
    );

    let changed = core
        .ingest(telegram_reaction_set("tg-1", &[(OWNER, "😂")]))
        .await
        .unwrap();
    assert_eq!(only_reaction(&changed).3, Some("😂"));

    let cleared = core
        .ingest(telegram_reaction_set("tg-1", &[]))
        .await
        .unwrap();
    let (dispatch, by_owner, _, emoji) = only_reaction(&cleared);
    assert!(by_owner);
    assert_eq!(emoji, None);
    let Action::React { reactor, .. } = &dispatch.action else {
        unreachable!();
    };
    assert_eq!(reactor, OWNER);
}

#[tokio::test]
async fn whatsapps_echo_of_an_applied_reaction_is_ignored() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    core.ingest(telegram_reaction("tg-1", OWNER, Some("❤️")))
        .await
        .unwrap();

    let echo = core
        .ingest(own_whatsapp_reaction(Some("❤️")))
        .await
        .unwrap();
    assert_eq!(
        echo,
        IngestOutcome::Ignored(IgnoreReason::RedundantReaction)
    );

    // A different reaction from the phone is the owner's own, and mirrors.
    let changed = core
        .ingest(own_whatsapp_reaction(Some("😂")))
        .await
        .unwrap();
    let (dispatch, by_owner, _, emoji) = only_reaction(&changed);
    assert_eq!(dispatch.to.connector_id, ConnectorId(TELEGRAM.into()));
    assert!(by_owner);
    assert_eq!(emoji, Some("😂"));
}

#[tokio::test]
async fn a_dead_lettered_owner_reaction_is_forgotten_at_its_target() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    let outcome = core
        .ingest(telegram_reaction("tg-1", OWNER, Some("❤️")))
        .await
        .unwrap();
    let dispatch_id = only_reaction(&outcome).0.id;

    core.fail_dispatch(dispatch_id, "rejected".into())
        .await
        .unwrap();

    let from_phone = core
        .ingest(own_whatsapp_reaction(Some("❤️")))
        .await
        .unwrap();
    assert_eq!(only_reaction(&from_phone).3, Some("❤️"));
}

// Plan 081: the owner's lone-emoji reply is a reaction.

/// A message typed in the direct rule's Telegram topic, replying to
/// `reply_to`, by `account` (as `owner_ids` name it).
fn topic_reply(native_id: &str, reply_to: &str, text: &str, account: &str) -> Event {
    let mut event = event_in(TELEGRAM, "-100123", Some("12"), native_id, Some(reply_to));
    let EventKind::NewMessage(message) = &mut event.payload else {
        unreachable!();
    };
    message.author_id = format!("telegram:user:{account}");
    message.author_account = Some(account.into());
    message.text = Some(text.into());
    event
}

fn telegram_edit(native_id: &str, text: &str) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(TELEGRAM.into()),
            address: ChatAddress::new("-100123", Some("12".into())),
            chat_title: None,
        },
        payload: EventKind::Edit {
            native_id: native_id.into(),
            text: Some(text.into()),
        },
    }
}

fn telegram_delete(native_id: &str) -> Event {
    Event {
        source: SourceAddress {
            connector_id: ConnectorId(TELEGRAM.into()),
            address: ChatAddress::new("-100123", Some("12".into())),
            chat_title: None,
        },
        payload: EventKind::Delete {
            native_id: native_id.into(),
        },
    }
}

#[tokio::test]
async fn the_owners_lone_emoji_reply_is_their_reaction_and_not_a_message() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;

    let outcome = core
        .ingest(topic_reply("tg-9", "tg-1", "🫠", OWNER))
        .await
        .unwrap();
    let (dispatch, by_owner, author, emoji) = only_reaction(&outcome);
    assert_eq!(dispatch.to.connector_id, ConnectorId(WHATSAPP.into()));
    assert!(by_owner);
    assert_eq!(emoji, Some("🫠"));
    assert_eq!(author.author_id, "alice");
    let Action::React {
        native_id, confirm, ..
    } = &dispatch.action
    else {
        unreachable!();
    };
    assert_eq!(native_id, "wa-1");
    assert_eq!(
        confirm.as_ref().map(|reply| reply.native_id.as_str()),
        Some("tg-9")
    );
}

#[tokio::test]
async fn an_acknowledged_reply_reaction_marks_the_reply_delivered() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    let outcome = core
        .ingest(topic_reply("tg-9", "tg-1", "🫠", OWNER))
        .await
        .unwrap();
    let reaction = only_reaction(&outcome).0.id;

    let follow_ups = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: reaction,
            native_id: "wa-1".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    let [signal] = follow_ups.as_slice() else {
        panic!("one delivered signal expected: {follow_ups:?}");
    };
    assert_eq!(signal.to.connector_id, ConnectorId(TELEGRAM.into()));
    assert!(matches!(
        &signal.action,
        Action::Receipt { native_id, kind: ReceiptKind::Delivered, .. } if native_id == "tg-9"
    ));
}

#[tokio::test]
async fn other_emoji_replies_stay_messages() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    for (native_id, text, account) in [
        ("tg-7", "🫠", "stranger"),
        ("tg-8", "🫠 nice", OWNER),
        ("tg-9", "🫠🫠", OWNER),
    ] {
        let outcome = core
            .ingest(topic_reply(native_id, "tg-1", text, account))
            .await
            .unwrap();
        assert!(
            matches!(&outcome.dispatches()[0].action, Action::Send { .. }),
            "{text:?} by {account} should be sent: {outcome:?}"
        );
    }
}

#[tokio::test]
async fn editing_or_deleting_the_reply_changes_or_clears_the_reaction() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    core.ingest(topic_reply("tg-9", "tg-1", "🫠", OWNER))
        .await
        .unwrap();

    let edited = core.ingest(telegram_edit("tg-9", "🥲")).await.unwrap();
    assert_eq!(only_reaction(&edited).3, Some("🥲"));

    let deleted = core.ingest(telegram_delete("tg-9")).await.unwrap();
    let (dispatch, by_owner, _, emoji) = only_reaction(&deleted);
    assert_eq!(dispatch.to.connector_id, ConnectorId(WHATSAPP.into()));
    assert!(by_owner);
    assert_eq!(emoji, None);
}

#[tokio::test]
async fn editing_the_reply_to_text_clears_the_reaction_and_sends_nothing_else() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    core.ingest(topic_reply("tg-9", "tg-1", "🫠", OWNER))
        .await
        .unwrap();

    let edited = core
        .ingest(telegram_edit("tg-9", "never mind"))
        .await
        .unwrap();
    assert_eq!(only_reaction(&edited).3, None);
}

#[tokio::test]
async fn the_latest_reaction_wins_and_clearing_an_older_one_keeps_it() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    core.ingest(telegram_reaction_set("tg-1", &[(OWNER, "❤️")]))
        .await
        .unwrap();
    let replied = core
        .ingest(topic_reply("tg-9", "tg-1", "🫠", OWNER))
        .await
        .unwrap();
    assert_eq!(only_reaction(&replied).3, Some("🫠"));

    // The native ❤️ goes; WhatsApp shows the reply's 🫠, which stays.
    let unreacted = core
        .ingest(telegram_reaction_set("tg-1", &[]))
        .await
        .unwrap();
    assert!(unreacted.dispatches().is_empty(), "{unreacted:?}");

    // The reply goes; its 🫠 was WhatsApp's reaction, so it is cleared.
    let deleted = core.ingest(telegram_delete("tg-9")).await.unwrap();
    assert_eq!(only_reaction(&deleted).3, None);
}

#[tokio::test]
async fn a_reaction_set_never_clears_a_reply_reaction() {
    let core = core_with_owner(vec![direct_rule()]);
    mirrored_whatsapp_message(&core).await;
    core.ingest(topic_reply("tg-9", "tg-1", "🫠", OWNER))
        .await
        .unwrap();

    let set = core
        .ingest(telegram_reaction_set("tg-1", &[]))
        .await
        .unwrap();
    assert_eq!(set, IngestOutcome::Ignored(IgnoreReason::RedundantReaction));
}

// ---- Plan 083: receive-only chats ----

/// The mirror rule's core with its topic provisioned (thread 777), built on
/// `config`.
async fn provisioned_mirror(config: RoutingConfig) -> Core<InMemoryStorage> {
    let core = Core::new(Arc::new(InMemoryStorage::default()), Arc::new(config));
    let (dispatches, _) = routed(
        core.ingest(event(WHATSAPP, GROUP, "wa-1", None))
            .await
            .unwrap(),
    );
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: dispatches[0].id,
        native_id: "tg-card".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();
    core
}

/// The group reported as one this account may not post in (an admin-only
/// group as a member).
async fn report_receive_only(core: &Core<InMemoryStorage>) {
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![ChatUpdate {
            posting: Some(wuphf_model::ChatPosting {
                messages: false,
                reactions: true,
                private_replies: false,
                posts: false,
                audience: None,
                post_edits: false,
            }),
            ..ChatUpdate::new(GROUP)
        }],
        true,
    )
    .await
    .unwrap();
}

/// The refusal note on the owner's copy in the topic, and nothing else.
fn assert_refused(dispatches: &[Dispatch], native_id: &str) {
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    let refusal = &dispatches[0];
    assert_eq!(refusal.to.connector_id, ConnectorId(TELEGRAM.into()));
    assert_eq!(refusal.to.address.thread_id.as_deref(), Some("777"));
    match &refusal.action {
        Action::Refuse {
            native_id: refused,
            reason,
            note_ttl_secs,
            ..
        } => {
            assert_eq!(refused, native_id);
            assert!(reason.contains("receive-only"), "{reason}");
            assert_eq!(*note_ttl_secs, 30);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_message_into_a_receive_only_chat_is_refused_with_a_note() {
    let core = provisioned_mirror(RoutingConfig::new(vec![mirror_rule()], connectors())).await;
    report_receive_only(&core).await;

    let (dispatches, blocked) = routed(
        core.ingest(event_in(TELEGRAM, MIRROR_CHAT, Some("777"), "tg-9", None))
            .await
            .unwrap(),
    );
    assert_eq!(blocked, 0);
    assert_refused(&dispatches, "tg-9");
    // The refusal records no copy: nothing reached WhatsApp.
    let refusal = &dispatches[0];
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: refusal.id,
        native_id: "tg-9".into(),
        thread_id: None,
    })
    .await
    .unwrap();

    // An edit of it is refused too.
    let (dispatches, _) = routed(
        core.ingest(edit_event(
            TELEGRAM,
            MIRROR_CHAT,
            Some("777"),
            "tg-9",
            Some("again"),
        ))
        .await
        .unwrap(),
    );
    assert_refused(&dispatches, "tg-9");
}

#[tokio::test]
async fn a_receive_only_chat_still_mirrors_into_its_topic() {
    let core = provisioned_mirror(RoutingConfig::new(vec![mirror_rule()], connectors())).await;
    report_receive_only(&core).await;
    let (dispatches, _) = routed(
        core.ingest(event(WHATSAPP, GROUP, "wa-2", None))
            .await
            .unwrap(),
    );
    assert_eq!(dispatches.len(), 1);
    assert!(matches!(dispatches[0].action, Action::Send { .. }));
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(TELEGRAM.into()));
}

#[tokio::test]
async fn the_owner_can_mark_a_chat_receive_only_by_its_topic() {
    let topic = ChatRef {
        connector_id: ConnectorId(TELEGRAM.into()),
        address: ChatAddress::new(MIRROR_CHAT, Some("777".into())),
    };
    let core = provisioned_mirror(
        RoutingConfig::new(vec![mirror_rule()], connectors()).with_receive_only(vec![topic]),
    )
    .await;
    let (dispatches, _) = routed(
        core.ingest(event_in(TELEGRAM, MIRROR_CHAT, Some("777"), "tg-9", None))
            .await
            .unwrap(),
    );
    assert_refused(&dispatches, "tg-9");
}

#[tokio::test]
async fn an_open_chat_takes_the_owners_message() {
    let core = provisioned_mirror(RoutingConfig::new(vec![mirror_rule()], connectors())).await;
    let (dispatches, _) = routed(
        core.ingest(event_in(TELEGRAM, MIRROR_CHAT, Some("777"), "tg-9", None))
            .await
            .unwrap(),
    );
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(WHATSAPP.into()));
    assert!(matches!(dispatches[0].action, Action::Send { .. }));
}

/// A delete of a mirror copy, with and without `mirror_deletes_kept`.
async fn mirror_delete(keep: bool) -> Vec<Dispatch> {
    let mut config = RoutingConfig::new(vec![mirror_rule()], connectors());
    if keep {
        config = config.with_mirror_deletes_kept([ConnectorId(WHATSAPP.into())]);
    }
    let core = provisioned_mirror(config).await;
    let (dispatches, _) = routed(
        core.ingest(event_in(TELEGRAM, MIRROR_CHAT, Some("777"), "tg-9", None))
            .await
            .unwrap(),
    );
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: dispatches[0].id,
        native_id: "wa-9".into(),
        thread_id: None,
    })
    .await
    .unwrap();
    let delete = Event {
        source: SourceAddress {
            connector_id: ConnectorId(TELEGRAM.into()),
            address: ChatAddress::new(MIRROR_CHAT, Some("777".into())),
            chat_title: None,
        },
        payload: EventKind::Delete {
            native_id: "tg-9".into(),
        },
    };
    routed(core.ingest(delete).await.unwrap()).0
}

#[tokio::test]
async fn deleting_a_mirror_copy_deletes_the_source_message_by_default() {
    let dispatches = mirror_delete(false).await;
    assert_eq!(dispatches.len(), 1);
    assert!(
        matches!(&dispatches[0].action, Action::Delete { native_id, .. } if native_id == "wa-9")
    );
}

#[tokio::test]
async fn mirror_deletes_kept_leaves_the_source_message() {
    assert!(mirror_delete(true).await.is_empty());
}

#[test]
fn receive_only_settings_must_name_configured_connectors() {
    let unknown = ChatRef {
        connector_id: ConnectorId("nope".into()),
        address: ChatAddress::new("x", None),
    };
    let config =
        RoutingConfig::new(vec![mirror_rule()], connectors()).with_receive_only(vec![unknown]);
    assert!(matches!(
        config.validate(),
        Err(crate::RoutingConfigError::UnknownReceiveOnlyConnector(_))
    ));
}

// ---- Plan 083: replies that quote another chat ----

const BOB: &str = "15550001111@s.whatsapp.net";
const ALICE: &str = "15550002222@s.whatsapp.net";
const STATUS: &str = "status@broadcast";

/// Every WhatsApp chat into the one Telegram forum.
fn mirror_everything() -> Core<InMemoryStorage> {
    let rule = Rule {
        id: RuleId("wa-all".into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chats_filter: ChatFilter::All,
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress::new(MIRROR_CHAT, None),
        },
    };
    core(vec![rule])
}

/// A WhatsApp message by `author` in `chat`, mirrored into a new topic
/// `thread` as Telegram message `tg_native`.
async fn mirror_into_topic(
    core: &Core<InMemoryStorage>,
    chat: &str,
    author: &str,
    wa_native: &str,
    thread: &str,
    tg_native: &str,
) {
    let mut event = event(WHATSAPP, chat, wa_native, None);
    if let EventKind::NewMessage(message) = &mut event.payload {
        message.author_id = author.into();
        message.participant = Some(author.into());
    }
    let (dispatches, _) = routed(core.ingest(event).await.unwrap());
    let replayed = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: dispatches[0].id,
            native_id: format!("card-{thread}"),
            thread_id: Some(thread.into()),
        })
        .await
        .unwrap();
    let send = replayed
        .iter()
        .find(|dispatch| matches!(dispatch.action, Action::Send { .. }))
        .expect("the message replays once the topic exists");
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: send.id,
        native_id: tg_native.into(),
        thread_id: None,
    })
    .await
    .unwrap();
}

/// The owner's reply in topic `thread` quoting Telegram message `quoted`.
async fn reply_in_topic(core: &Core<InMemoryStorage>, thread: &str, quoted: &str) -> Vec<Dispatch> {
    routed(
        core.ingest(event_in(
            TELEGRAM,
            MIRROR_CHAT,
            Some(thread),
            "tg-reply",
            Some(quoted),
        ))
        .await
        .unwrap(),
    )
    .0
}

async fn story_feed() -> Core<InMemoryStorage> {
    let core = mirror_everything();
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![ChatUpdate {
            posting: Some(wuphf_model::ChatPosting {
                messages: false,
                reactions: true,
                private_replies: true,
                posts: false,
                audience: None,
                post_edits: false,
            }),
            ..ChatUpdate::new(STATUS)
        }],
        true,
    )
    .await
    .unwrap();
    mirror_into_topic(&core, STATUS, BOB, "wa-story", "100", "tg-story").await;
    mirror_into_topic(&core, BOB, BOB, "wa-bob", "200", "tg-bob").await;
    mirror_into_topic(&core, ALICE, ALICE, "wa-alice", "300", "tg-alice").await;
    core
}

#[tokio::test]
async fn quoting_a_story_in_its_posters_topic_is_a_status_reply() {
    let core = story_feed().await;
    let dispatches = reply_in_topic(&core, "200", "tg-story").await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    assert_eq!(dispatches[0].to.address.chat_id, BOB);
    match &dispatches[0].action {
        Action::Send {
            reply: Some(quote), ..
        } => {
            assert_eq!(quote.native_id, "wa-story");
            assert_eq!(quote.chat_id.as_deref(), Some(STATUS));
            assert!(!quote.authored_by_bridge);
        }
        other => panic!("expected a quoting send, got {other:?}"),
    }
}

#[tokio::test]
async fn quoting_a_story_anywhere_else_is_refused() {
    let core = story_feed().await;
    let dispatches = reply_in_topic(&core, "300", "tg-story").await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    assert_eq!(dispatches[0].to.connector_id, ConnectorId(TELEGRAM.into()));
    match &dispatches[0].action {
        Action::Refuse {
            native_id, reason, ..
        } => {
            assert_eq!(native_id, "tg-reply");
            assert!(reason.contains("poster"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn quoting_a_group_message_in_a_members_dm_keeps_the_quote() {
    let core = mirror_everything();
    mirror_into_topic(&core, GROUP, BOB, "wa-group", "100", "tg-group").await;
    mirror_into_topic(&core, BOB, BOB, "wa-bob", "200", "tg-bob").await;
    let dispatches = reply_in_topic(&core, "200", "tg-group").await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    match &dispatches[0].action {
        Action::Send {
            reply: Some(quote), ..
        } => {
            assert_eq!(quote.native_id, "wa-group");
            assert_eq!(quote.chat_id.as_deref(), Some(GROUP));
        }
        other => panic!("expected a quoting send, got {other:?}"),
    }
}

#[tokio::test]
async fn a_quote_of_another_chat_named_by_the_event_resolves_there_only() {
    let core = mirror_everything();
    mirror_into_topic(&core, BOB, BOB, "wa-bob", "200", "tg-bob").await;
    // The same native id in another Telegram chat is not this one.
    let mut event = event_in(
        TELEGRAM,
        MIRROR_CHAT,
        Some("200"),
        "tg-reply",
        Some("tg-bob"),
    );
    if let EventKind::NewMessage(message) = &mut event.payload {
        message.reply_to_chat_id = Some("-100999".into());
    }
    let (dispatches, _) = routed(core.ingest(event).await.unwrap());
    assert!(matches!(
        &dispatches[0].action,
        Action::Send { reply: None, .. }
    ));
}

// ---- Plan 084: posting stories ----

/// The Status feed mirrored into topic 100, taking posts, with the owner's
/// message "tg-mine" typed there (and refused, 083).
async fn story_topic() -> Core<InMemoryStorage> {
    posting_topic(
        STATUS,
        "100",
        wuphf_model::ChatPosting {
            messages: false,
            reactions: true,
            private_replies: true,
            posts: true,
            audience: Some("My contacts".into()),
            post_edits: false,
        },
    )
    .await
}

/// WhatsApp `chat` mirrored into topic `thread` with `posting`, and the
/// owner's message "tg-mine" typed there (and refused, 083).
async fn posting_topic(
    chat: &str,
    thread: &str,
    posting: wuphf_model::ChatPosting,
) -> Core<InMemoryStorage> {
    let rule = Rule {
        id: RuleId("wa-all".into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chats_filter: ChatFilter::All,
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress::new(MIRROR_CHAT, None),
        },
    };
    let core = core_with_owner(vec![rule]);
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![ChatUpdate {
            posting: Some(posting),
            ..ChatUpdate::new(chat)
        }],
        true,
    )
    .await
    .unwrap();
    mirror_into_topic(&core, chat, BOB, "wa-story", thread, "tg-story").await;
    let refused = owner_in_topic(&core, thread, "tg-mine", "my story", None).await;
    assert!(matches!(refused[0].action, Action::Refuse { .. }));
    core
}

/// The owner's message `text` in topic `thread`, replying to `reply_to`.
async fn owner_in_topic(
    core: &Core<InMemoryStorage>,
    thread: &str,
    native_id: &str,
    text: &str,
    reply_to: Option<&str>,
) -> Vec<Dispatch> {
    let mut event = event_in(TELEGRAM, MIRROR_CHAT, Some(thread), native_id, reply_to);
    if let EventKind::NewMessage(message) = &mut event.payload {
        message.text = Some(text.into());
        message.author_account = Some(OWNER.into());
        message.timestamp_ms = 1_000;
    }
    routed(core.ingest(event).await.unwrap()).0
}

fn status_topic() -> ChatAddress {
    ChatAddress::new(MIRROR_CHAT, Some("100".into()))
}

fn assert_post(dispatch: &Dispatch, text: &str) {
    assert_eq!(dispatch.to.address.chat_id, STATUS);
    match &dispatch.action {
        Action::Post { message } => assert_eq!(message.text.as_deref(), Some(text)),
        other => panic!("expected a post, got {other:?}"),
    }
}

#[tokio::test]
async fn status_replying_to_a_message_opens_its_preview_then_posts_it() {
    let core = story_topic().await;
    let dispatches = owner_in_topic(&core, "100", "tg-cmd", "/status", Some("tg-mine")).await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    assert_eq!(dispatches[0].to.address.thread_id.as_deref(), Some("100"));
    assert!(matches!(&dispatches[0].action, Action::OpenPanel { actor } if actor == OWNER));

    let telegram = ConnectorId(TELEGRAM.into());
    let pending = core
        .pending_post_at(&telegram, &status_topic(), 2_000)
        .await
        .unwrap()
        .expect("a pending post in the topic");
    assert_eq!(pending.audience.as_deref(), Some("My contacts"));
    assert!(pending.caption.is_none());

    let PostOutcome::Posted(posted) = core.confirm_post(pending.id, 3_000).await.unwrap() else {
        panic!("confirmed");
    };
    assert_eq!(posted.len(), 1);
    assert_post(&posted[0], "my story");
    // Its acknowledgement marks the owner's message delivered (👍).
    let after = core
        .acknowledge_dispatch(DispatchAck {
            dispatch_id: posted[0].id,
            native_id: "wa-posted".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    assert!(after.iter().any(|dispatch| matches!(
        &dispatch.action,
        Action::Receipt { native_id, kind: ReceiptKind::Delivered, .. } if native_id == "tg-mine"
    )));
    // Confirmed once only.
    assert!(core.pending_post(pending.id, 3_000).is_none());
}

#[tokio::test]
async fn status_bang_posts_at_once_with_the_command_text_as_caption() {
    let core = story_topic().await;
    let dispatches = owner_in_topic(
        &core,
        "100",
        "tg-cmd",
        "/status! look at this",
        Some("tg-mine"),
    )
    .await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    assert_post(&dispatches[0], "look at this");
}

#[tokio::test]
async fn status_with_text_posts_the_text() {
    let core = story_topic().await;
    let dispatches = owner_in_topic(&core, "100", "tg-cmd", "/status! hello world", None).await;
    assert_post(&dispatches[0], "hello world");
}

#[tokio::test]
async fn a_post_command_with_nothing_to_post_is_refused() {
    let core = story_topic().await;
    let dispatches = owner_in_topic(&core, "100", "tg-cmd", "/status", None).await;
    assert!(
        matches!(&dispatches[0].action, Action::Refuse { native_id, reason, .. } if native_id == "tg-cmd" && reason.contains("Nothing to post"))
    );
}

#[tokio::test]
async fn a_post_command_where_nothing_takes_posts_is_refused() {
    let core = story_topic().await;
    mirror_into_topic(&core, BOB, BOB, "wa-bob", "200", "tg-bob").await;
    let dispatches = owner_in_topic(&core, "200", "tg-cmd", "/status! hi", None).await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    assert!(
        matches!(&dispatches[0].action, Action::Refuse { reason, .. } if reason.contains("takes no posts"))
    );
}

#[tokio::test]
async fn someone_elses_post_command_is_an_ordinary_refused_message() {
    let core = story_topic().await;
    let mut event = event_in(TELEGRAM, MIRROR_CHAT, Some("100"), "tg-cmd", None);
    if let EventKind::NewMessage(message) = &mut event.payload {
        message.text = Some("/status! hi".into());
    }
    let (dispatches, _) = routed(core.ingest(event).await.unwrap());
    assert!(
        matches!(&dispatches[0].action, Action::Refuse { reason, .. } if reason.contains("receive-only"))
    );
}

#[tokio::test]
async fn an_expired_pending_post_is_not_posted() {
    let core = story_topic().await;
    owner_in_topic(&core, "100", "tg-cmd", "/status", Some("tg-mine")).await;
    let telegram = ConnectorId(TELEGRAM.into());
    let late = 1_000 + PENDING_POST_TTL_MS + 1;
    assert!(
        core.pending_post_at(&telegram, &status_topic(), late)
            .await
            .unwrap()
            .is_none()
    );
    let id = core
        .pending_post_at(&telegram, &status_topic(), 2_000)
        .await
        .unwrap()
        .unwrap()
        .id;
    assert!(matches!(
        core.confirm_post(id, late).await.unwrap(),
        PostOutcome::Refused(_)
    ));
}

#[tokio::test]
async fn a_post_composed_in_the_panel_goes_to_the_topic_and_the_story() {
    let core = story_topic().await;
    let telegram = ConnectorId(TELEGRAM.into());
    let PostOutcome::Posted(dispatches) = core
        .compose_post(&telegram, &status_topic(), "from the panel", 5_000)
        .await
        .unwrap()
    else {
        panic!("composed");
    };
    assert_eq!(dispatches.len(), 2, "{dispatches:#?}");
    assert!(
        matches!(&dispatches[0].action, Action::Send { message, .. } if message.text.as_deref() == Some("from the panel"))
    );
    assert_eq!(dispatches[0].to.address.thread_id.as_deref(), Some("100"));
    assert_post(&dispatches[1], "from the panel");
}

// ---- Plan 084: story mutes and expiry ----

/// Every WhatsApp chat mirrored, with `stories`, and Status a story feed.
async fn stories_core(stories: crate::StoriesSettings) -> Core<InMemoryStorage> {
    let rule = Rule {
        id: RuleId("wa-all".into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chats_filter: ChatFilter::All,
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress::new(MIRROR_CHAT, None),
        },
    };
    let config = RoutingConfig::new(vec![rule], connectors()).with_stories(stories);
    let core = Core::new(Arc::new(InMemoryStorage::default()), Arc::new(config));
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![ChatUpdate {
            posting: Some(wuphf_model::ChatPosting {
                messages: false,
                reactions: true,
                private_replies: true,
                posts: false,
                audience: None,
                post_edits: false,
            }),
            ..ChatUpdate::new(STATUS)
        }],
        true,
    )
    .await
    .unwrap();
    core
}

fn story_by(author: &str, native_id: &str) -> Event {
    let mut event = event(WHATSAPP, STATUS, native_id, None);
    if let EventKind::NewMessage(message) = &mut event.payload {
        message.author_id = author.into();
        message.participant = Some(author.into());
    }
    event
}

fn muting(chat: &str) -> crate::StoriesSettings {
    crate::StoriesSettings {
        muted: vec![ChatRef {
            connector_id: ConnectorId(WHATSAPP.into()),
            address: ChatAddress::new(chat, None),
        }],
        ..crate::StoriesSettings::default()
    }
}

#[tokio::test]
async fn a_muted_persons_story_is_dropped_but_their_messages_are_not() {
    let core = stories_core(muting(BOB)).await;
    assert_eq!(
        core.ingest(story_by(BOB, "wa-story")).await.unwrap(),
        IngestOutcome::Ignored(IgnoreReason::Muted)
    );
    // Not stored: a later delete of it matches nothing.
    assert!(matches!(
        core.ingest(story_by(ALICE, "wa-other")).await.unwrap(),
        IngestOutcome::Routed { .. }
    ));
    let mut dm = event(WHATSAPP, BOB, "wa-dm", None);
    if let EventKind::NewMessage(message) = &mut dm.payload {
        message.author_id = BOB.into();
    }
    assert!(matches!(
        core.ingest(dm).await.unwrap(),
        IngestOutcome::Routed { .. }
    ));
}

#[tokio::test]
async fn with_stories_off_no_story_is_mirrored() {
    let core = stories_core(crate::StoriesSettings {
        mirror: false,
        ..crate::StoriesSettings::default()
    })
    .await;
    assert_eq!(
        core.ingest(story_by(ALICE, "wa-story")).await.unwrap(),
        IngestOutcome::Ignored(IgnoreReason::Muted)
    );
}

#[tokio::test]
async fn an_expired_storys_mirror_copy_is_deleted_once() {
    let core = stories_core(crate::StoriesSettings {
        expire_copies: true,
        ..crate::StoriesSettings::default()
    })
    .await;
    mirror_into_topic(&core, STATUS, BOB, "wa-story", "100", "tg-story").await;
    // The story's timestamp is 1.
    assert!(
        core.expire_stories(STORY_LIFETIME_MS)
            .await
            .unwrap()
            .is_empty()
    );
    let deletes = core.expire_stories(STORY_LIFETIME_MS + 2).await.unwrap();
    assert_eq!(deletes.len(), 1, "{deletes:#?}");
    assert_eq!(deletes[0].to.connector_id, ConnectorId(TELEGRAM.into()));
    assert!(
        matches!(&deletes[0].action, Action::Delete { native_id, .. } if native_id == "tg-story")
    );
    assert!(
        core.expire_stories(STORY_LIFETIME_MS + 3)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn story_copies_stay_while_expiry_is_off() {
    let core = stories_core(crate::StoriesSettings::default()).await;
    mirror_into_topic(&core, STATUS, BOB, "wa-story", "100", "tg-story").await;
    assert!(
        core.expire_stories(STORY_LIFETIME_MS * 2)
            .await
            .unwrap()
            .is_empty()
    );
}

// ---- Plan 085: channels ----

const CHANNEL: &str = "120363000000000001@newsletter";

/// An owned channel mirrored into topic 300: it takes posts and their edits.
async fn channel_topic() -> Core<InMemoryStorage> {
    posting_topic(
        CHANNEL,
        "300",
        wuphf_model::ChatPosting {
            messages: false,
            reactions: false,
            private_replies: false,
            posts: true,
            audience: None,
            post_edits: true,
        },
    )
    .await
}

/// `/post!`s the owner's "tg-mine" in `thread` and acknowledges the post as
/// "wa-posted"; the dispatches of editing "tg-mine" afterwards.
async fn post_then_edit(core: &Core<InMemoryStorage>, thread: &str) -> Vec<Dispatch> {
    let posted = owner_in_topic(core, thread, "tg-cmd", "/post!", Some("tg-mine")).await;
    assert_eq!(posted.len(), 1, "{posted:#?}");
    assert!(matches!(posted[0].action, Action::Post { .. }));
    core.acknowledge_dispatch(DispatchAck {
        dispatch_id: posted[0].id,
        native_id: "wa-posted".into(),
        thread_id: None,
    })
    .await
    .unwrap();
    let edit = edit_event(
        TELEGRAM,
        MIRROR_CHAT,
        Some(thread),
        "tg-mine",
        Some("fixed"),
    );
    routed(core.ingest(edit).await.unwrap()).0
}

#[tokio::test]
async fn an_edit_of_a_channel_post_edits_the_post() {
    let core = channel_topic().await;
    let dispatches = post_then_edit(&core, "300").await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    assert_eq!(dispatches[0].to.address.chat_id, CHANNEL);
    assert!(matches!(
        &dispatches[0].action,
        Action::Edit { native_id, .. } if native_id == "wa-posted"
    ));
}

#[tokio::test]
async fn an_edit_of_a_story_is_refused() {
    let core = story_topic().await;
    let dispatches = post_then_edit(&core, "100").await;
    assert_eq!(dispatches.len(), 1, "{dispatches:#?}");
    assert!(matches!(
        &dispatches[0].action,
        Action::Refuse { native_id, reason, .. }
            if native_id == "tg-mine" && reason.contains("cannot edit a post")
    ));
}

#[tokio::test]
async fn a_plain_message_in_a_channel_topic_is_refused() {
    let core = channel_topic().await;
    let dispatches = owner_in_topic(&core, "300", "tg-hi", "hello followers", None).await;
    assert!(matches!(
        &dispatches[0].action,
        Action::Refuse { native_id, .. } if native_id == "tg-hi"
    ));
}

// ---- Plan 085: communities ----

const COMMUNITY: &str = "999@g.us";

/// Every WhatsApp chat `filter` passes, mirrored into the forum.
fn community_rule(filter: ChatFilter) -> Rule {
    Rule {
        id: RuleId("wa-community".into()),
        flow: RuleKind::ManyToOne {
            source_connector: ConnectorId(WHATSAPP.into()),
            source_chats_filter: filter,
            destination_connector: ConnectorId(TELEGRAM.into()),
            destination_chat: ChatAddress::new(MIRROR_CHAT, None),
        },
    }
}

fn naming_the_community() -> Vec<ChatAddress> {
    vec![ChatAddress::new(COMMUNITY, None)]
}

/// `group` reported as a member of `community` ("" = of none).
async fn report_community(core: &Core<InMemoryStorage>, group: &str, community: &str) {
    core.update_chats(
        &ConnectorId(WHATSAPP.into()),
        vec![ChatUpdate {
            community: Some(wuphf_model::ChatCommunity {
                chat_id: community.into(),
                title: "Neighbours".into(),
            }),
            ..ChatUpdate::new(group)
        }],
        true,
    )
    .await
    .unwrap();
}

async fn mirrored(core: &Core<InMemoryStorage>, group: &str, native_id: &str) -> bool {
    !core
        .ingest(event(WHATSAPP, group, native_id, None))
        .await
        .unwrap()
        .dispatches()
        .is_empty()
}

#[tokio::test]
async fn a_filter_naming_a_community_mirrors_its_groups_only() {
    let core = core(vec![community_rule(ChatFilter::Only {
        chats: naming_the_community(),
    })]);
    report_community(&core, GROUP, COMMUNITY).await;
    assert!(mirrored(&core, GROUP, "wa-1").await);
    assert!(!mirrored(&core, OTHER_GROUP, "wa-2").await);
}

#[tokio::test]
async fn a_group_that_leaves_its_community_stops_mirroring() {
    let core = core(vec![community_rule(ChatFilter::Only {
        chats: naming_the_community(),
    })]);
    report_community(&core, GROUP, COMMUNITY).await;
    assert!(mirrored(&core, GROUP, "wa-1").await);
    report_community(&core, GROUP, "").await;
    assert!(!mirrored(&core, GROUP, "wa-2").await);
}

#[tokio::test]
async fn a_deny_list_naming_a_community_excludes_its_groups() {
    let core = core(vec![community_rule(ChatFilter::Not {
        chats: naming_the_community(),
    })]);
    report_community(&core, GROUP, COMMUNITY).await;
    assert!(!mirrored(&core, GROUP, "wa-1").await);
    assert!(mirrored(&core, OTHER_GROUP, "wa-2").await);
}

#[tokio::test]
async fn receive_only_naming_a_community_covers_its_groups() {
    let community = ChatRef {
        connector_id: ConnectorId(WHATSAPP.into()),
        address: ChatAddress::new(COMMUNITY, None),
    };
    let core = provisioned_mirror(
        RoutingConfig::new(vec![mirror_rule()], connectors()).with_receive_only(vec![community]),
    )
    .await;
    report_community(&core, GROUP, COMMUNITY).await;
    let (dispatches, _) = routed(
        core.ingest(event_in(TELEGRAM, MIRROR_CHAT, Some("777"), "tg-9", None))
            .await
            .unwrap(),
    );
    assert_refused(&dispatches, "tg-9");
}
