use super::*;
use std::collections::HashMap;
use wuphf_model::{
    Attachment, AttachmentKind, ConnectorCapabilities, DispatchPayload, InboundMessage, MediaBlob,
    MediaId, Platform, Rule, RuleId, RuleSide,
};
use wuphf_storage::InMemoryStorage;

const WHATSAPP: &str = "whatsapp-main";
const TELEGRAM: &str = "telegram-main";

fn connectors() -> HashMap<ConnectorId, ConnectorInfo> {
    HashMap::from([
        (
            ConnectorId(WHATSAPP.into()),
            ConnectorInfo {
                platform: Platform::WhatsApp,
                capabilities: ConnectorCapabilities::default(),
                delivery: Default::default(),
            },
        ),
        (
            ConnectorId(TELEGRAM.into()),
            ConnectorInfo {
                platform: Platform::Telegram,
                capabilities: ConnectorCapabilities {
                    supports_threads: true,
                    supports_topic_create: true,
                },
                delivery: Default::default(),
            },
        ),
    ])
}

fn direct_rule() -> Rule {
    Rule {
        id: RuleId("direct".into()),
        sides: vec![
            RuleSide {
                connector: ConnectorId(WHATSAPP.into()),
                kind: SideKind::Single {
                    chat_id: "12345@s.whatsapp.net".into(),
                    thread_id: None,
                },
            },
            RuleSide {
                connector: ConnectorId(TELEGRAM.into()),
                kind: SideKind::Single {
                    chat_id: "-100123".into(),
                    thread_id: Some("12".into()),
                },
            },
        ],
    }
}

fn mirror_rule() -> Rule {
    Rule {
        id: RuleId("wa-mirror".into()),
        sides: vec![
            RuleSide {
                connector: ConnectorId(WHATSAPP.into()),
                kind: SideKind::Source {
                    include: vec!["111@g.us".into()],
                    exclude: vec![],
                },
            },
            RuleSide {
                connector: ConnectorId(TELEGRAM.into()),
                kind: SideKind::Mirror {
                    container: "-1001234567".into(),
                },
            },
        ],
    }
}

fn core(rules: Vec<Rule>) -> Core<InMemoryStorage> {
    let storage = Arc::new(InMemoryStorage::default());
    let config = Arc::new(RoutingConfig::new(rules, connectors()));
    Core::new(storage, config)
}

fn event(
    connector: &str,
    chat_id: &str,
    native_id: &str,
    reply_to: Option<&str>,
) -> NormalizedEvent {
    event_in(connector, chat_id, None, native_id, reply_to)
}

fn event_in(
    connector: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    native_id: &str,
    reply_to: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
        source: SourceAddress {
            connector_id: ConnectorId(connector.into()),
            address: PlatformAddress::new(chat_id, thread_id.map(str::to_owned)),
            chat_title: Some("Title".into()),
        },
        payload: EventPayload::MessageCreate(InboundMessage {
            platform_message_id: native_id.into(),
            sender: "alice".into(),
            participant: None,
            sender_name: None,
            text: Some("hello".into()),
            attachments: vec![],
            reply_to_platform_message_id: reply_to.map(str::to_owned),
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
    let command = &dispatches[0].command;
    assert_eq!(command.target_connector, ConnectorId(TELEGRAM.into()));
    assert_eq!(
        command.target.address,
        PlatformAddress::new("-100123", Some("12".into()))
    );
    assert!(command.target.provision_thread.is_none());
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
    let provision = dispatch
        .command
        .target
        .provision_thread
        .as_ref()
        .expect("first mirror send provisions a thread");
    assert_eq!(provision.container, "-1001234567");
    assert_eq!(provision.title, "Title");

    core.acknowledge_dispatch(DispatchAcknowledgement {
        dispatch_id: dispatch.id,
        platform_message_id: "tg-1".into(),
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
    assert_eq!(back.command.target_connector, ConnectorId(WHATSAPP.into()));
    assert_eq!(back.command.target.address.chat_id, "111@g.us");

    // The next source message reuses the thread without provisioning.
    let again = core
        .ingest(event(WHATSAPP, "111@g.us", "wa-2", None))
        .await
        .unwrap();
    let resend = &again.dispatches()[0];
    assert_eq!(
        resend.command.target.address,
        PlatformAddress::new("-1001234567", Some("777".into()))
    );
    assert!(resend.command.target.provision_thread.is_none());
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
    assert!(dispatches[0].command.target.provision_thread.is_some());

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
        .acknowledge_dispatch(DispatchAcknowledgement {
            dispatch_id: dispatches[0].id,
            platform_message_id: "tg-1".into(),
            thread_id: Some("777".into()),
        })
        .await
        .unwrap();
    assert_eq!(replayed.len(), 1);
    let command = &replayed[0].command;
    assert_eq!(command.target_connector, ConnectorId(TELEGRAM.into()));
    assert!(command.target.provision_thread.is_none());
    assert_eq!(command.target.address.thread_id, Some("777".into()));
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
    let telegram = dispatches[0].command.target_endpoint;
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
    let telegram = dispatches[0].command.target_endpoint;
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
    let DispatchPayload::MessageCreate {
        message,
        reply_context,
    } = &reply.dispatches()[0].command.payload
    else {
        panic!("reply should route as a message create");
    };
    assert_eq!(message.reply_to, Some(parent_id));
    let reply_context = reply_context.as_ref().expect("reply context resolved");
    assert_eq!(reply_context.message_id, parent_id);
    assert_eq!(reply_context.platform_message_id, "tg-parent");
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
    let IngestOutcome::Routed { routing, .. } = reply else {
        panic!("reply should be stored while delivery is blocked");
    };
    assert!(routing.dispatches.is_empty());
    assert_eq!(routing.blocked.len(), 1);
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
    core.acknowledge_dispatch(DispatchAcknowledgement {
        dispatch_id: dispatches[0].id,
        platform_message_id: "tg-1".into(),
        thread_id: Some("777".into()),
    })
    .await
    .unwrap();

    assert!(
        core.migrate_identity(
            ConnectorId(WHATSAPP.into()),
            PlatformAddress::new("111@g.us", None),
            PlatformAddress::new("111@lid", None),
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
    assert_eq!(
        dispatches[0].command.target_connector,
        ConnectorId(TELEGRAM.into())
    );
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
    };
    let media_event = |attachment: Attachment| {
        let mut event = event(WHATSAPP, "12345@s.whatsapp.net", "wa-media", None);
        let EventPayload::MessageCreate(inbound) = &mut event.payload else {
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
) -> NormalizedEvent {
    NormalizedEvent {
        source: SourceAddress {
            connector_id: ConnectorId(connector.into()),
            address: PlatformAddress::new(chat_id, thread_id.map(str::to_owned)),
            chat_title: None,
        },
        payload: EventPayload::MessageEdit {
            platform_message_id: native_id.into(),
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
    let telegram = dispatches[0].command.target_endpoint;
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
    assert_eq!(
        dispatch.command.target_connector,
        ConnectorId(TELEGRAM.into())
    );
    let DispatchPayload::MessageEdit {
        message,
        target_platform_message_id,
        ..
    } = &dispatch.command.payload
    else {
        panic!("edit should route as a message edit");
    };
    assert_eq!(target_platform_message_id, "tg-1");
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
        routing,
        dispatches,
        ..
    } = edited
    else {
        panic!("edit should be stored while delivery is blocked");
    };
    assert!(dispatches.is_empty());
    assert_eq!(routing.blocked.len(), 1);

    let released = core
        .acknowledge_dispatch(DispatchAcknowledgement {
            dispatch_id: create_dispatch.id,
            platform_message_id: "tg-1".into(),
            thread_id: None,
        })
        .await
        .unwrap();
    let edit = released
        .iter()
        .find(|dispatch| {
            matches!(
                dispatch.command.payload,
                DispatchPayload::MessageEdit { .. }
            )
        })
        .expect("acknowledging the create releases the blocked edit");
    let DispatchPayload::MessageEdit {
        target_platform_message_id,
        ..
    } = &edit.command.payload
    else {
        unreachable!();
    };
    assert_eq!(target_platform_message_id, "tg-1");
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
