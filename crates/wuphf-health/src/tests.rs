use super::*;
use wuphf_model::{ConnectorCapabilities, Platform};

const TG: &str = "telegram-main";
const WA: &str = "whatsapp-jio";
const GROUP: &str = "4350116464";
const SOURCE: &str = "1203@g.us";

fn id(connector: &str) -> ConnectorId {
    ConnectorId(connector.into())
}

fn connectors() -> Vec<Connector> {
    vec![
        Connector {
            id: id(TG),
            platform: Platform::Telegram,
            capabilities: ConnectorCapabilities {
                supports_threads: true,
                supports_topic_create: true,
                supports_disappearing_timers: false,
            },
        },
        Connector {
            id: id(WA),
            platform: Platform::WhatsApp,
            capabilities: ConnectorCapabilities::default(),
        },
    ]
}

fn mirror(filter: ChatFilter) -> Rule {
    Rule {
        id: RuleId("mirror".into()),
        flow: RuleKind::ManyToOne {
            source_connector: id(WA),
            source_chats_filter: filter,
            destination_connector: id(TG),
            destination_chat: ChatAddress::new(GROUP, None),
        },
    }
}

fn healthy() -> ChatFacts {
    ChatFacts {
        exists: Fact::Yes,
        member: Fact::Yes,
        can_send: Fact::Yes,
        forum: Fact::Yes,
        can_create_topics: Fact::Yes,
        can_pin: Fact::Yes,
        can_delete_others: Fact::Yes,
        detail: String::new(),
    }
}

fn facts(entries: &[(&str, &str, ChatFacts)]) -> ByChat<ChatFacts> {
    entries
        .iter()
        .map(|(connector, chat, facts)| ((id(connector), (*chat).to_owned()), facts.clone()))
        .collect()
}

fn run(rules: &[Rule], facts: &ByChat<ChatFacts>) -> Vec<Problem> {
    let connectors = connectors();
    evaluate(&Observed {
        rules,
        connectors: &connectors,
        facts,
        ..Observed::default()
    })
}

fn keys(problems: &[Problem]) -> Vec<&str> {
    problems
        .iter()
        .map(|problem| problem.key.as_str())
        .collect()
}

#[test]
fn probe_targets_are_destinations_and_named_sources() {
    let everything = mirror(ChatFilter::All);
    let named = Rule {
        id: RuleId("named".into()),
        ..mirror(ChatFilter::Only {
            chats: vec![ChatAddress::new(SOURCE, None)],
        })
    };
    let targets = probe_targets(&[everything, named]);
    assert_eq!(
        targets.get(&id(TG)).unwrap().iter().collect::<Vec<_>>(),
        [GROUP]
    );
    assert_eq!(
        targets.get(&id(WA)).unwrap().iter().collect::<Vec<_>>(),
        [SOURCE]
    );
}

#[test]
fn a_healthy_or_unknown_chat_has_no_problems() {
    let rules = [mirror(ChatFilter::All)];
    assert!(run(&rules, &facts(&[(TG, GROUP, healthy())])).is_empty());
    assert!(run(&rules, &facts(&[(TG, GROUP, ChatFacts::default())])).is_empty());
    // Not probed at all.
    assert!(run(&rules, &ByChat::new()).is_empty());
}

#[test]
fn a_target_without_topics_pauses_forward() {
    let rules = [mirror(ChatFilter::All)];
    let not_forum = ChatFacts {
        forum: Fact::No,
        can_create_topics: Fact::No,
        ..healthy()
    };
    let problems = run(&rules, &facts(&[(TG, GROUP, not_forum)]));
    assert_eq!(
        keys(&problems),
        [
            "target-cannot-create-topics:mirror:telegram-main:4350116464",
            "target-not-forum:mirror:telegram-main:4350116464",
        ]
    );
    let pause = problems[1].pause().unwrap();
    assert_eq!(
        pause,
        &HealthPause {
            rule: RuleId("mirror".into()),
            direction: FlowDirection::Forward,
            connector: id(TG),
            chat_id: GROUP.into(),
        }
    );
    assert!(
        problems[1].text.contains("turn on Topics"),
        "{}",
        problems[1].text
    );
}

#[test]
fn topics_are_not_needed_where_no_threads_are_used() {
    // A one-to-one into the group itself, not into a topic.
    let direct = Rule {
        id: RuleId("direct".into()),
        flow: RuleKind::OneToOne {
            source_connector: id(WA),
            source_chat: ChatAddress::new(SOURCE, None),
            destination_connector: id(TG),
            destination_chat: ChatAddress::new(GROUP, None),
        },
    };
    let not_forum = ChatFacts {
        forum: Fact::No,
        can_create_topics: Fact::No,
        ..healthy()
    };
    assert!(run(&[direct], &facts(&[(TG, GROUP, not_forum)])).is_empty());
}

#[test]
fn an_admin_only_source_pauses_back_only() {
    let rules = [mirror(ChatFilter::Only {
        chats: vec![ChatAddress::new(SOURCE, None)],
    })];
    let announce_only = ChatFacts {
        can_send: Fact::No,
        detail: "only admins can send".into(),
        ..healthy()
    };
    let problems = run(
        &rules,
        &facts(&[(TG, GROUP, healthy()), (WA, SOURCE, announce_only)]),
    );
    assert_eq!(
        keys(&problems),
        ["source-cannot-send:mirror:whatsapp-jio:1203@g.us"]
    );
    let pause = problems[0].pause().unwrap();
    assert_eq!(pause.direction, FlowDirection::Back);
    assert_eq!(pause.connector, id(WA));
    assert!(problems[0].text.contains("only admins can send"));
    assert!(problems[0].text.ends_with('.'));
}

#[test]
fn a_missing_pin_right_only_warns_and_only_when_pins_sync() {
    let rules = [mirror(ChatFilter::All)];
    let no_pin = facts(&[(
        TG,
        GROUP,
        ChatFacts {
            can_pin: Fact::No,
            ..healthy()
        },
    )]);
    assert!(run(&rules, &no_pin).is_empty());
    let connectors = connectors();
    let problems = evaluate(&Observed {
        rules: &rules,
        connectors: &connectors,
        facts: &no_pin,
        pins_synced: true,
        ..Observed::default()
    });
    assert_eq!(
        keys(&problems),
        ["target-cannot-pin:mirror:telegram-main:4350116464"]
    );
    assert_eq!(problems[0].severity, Severity::Warn);
}

#[test]
fn titles_name_the_chat() {
    let rules = [mirror(ChatFilter::All)];
    let missing = facts(&[(
        TG,
        GROUP,
        ChatFacts {
            exists: Fact::No,
            ..ChatFacts::default()
        },
    )]);
    let titles = BTreeMap::from([((id(TG), GROUP.to_owned()), "WA jio".to_owned())]);
    let connectors = connectors();
    let problems = evaluate(&Observed {
        rules: &rules,
        connectors: &connectors,
        facts: &missing,
        titles: &titles,
        ..Observed::default()
    });
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].text.contains("\"WA jio\" (4350116464)"),
        "{}",
        problems[0].text
    );
}

#[test]
fn wiring_and_tools_warn() {
    let tools = BTreeMap::from([(
        "mediaconv".to_owned(),
        vec![
            ToolStatus {
                name: "ffmpeg".into(),
                available: false,
                needed_for: "animated stickers to WhatsApp".into(),
            },
            ToolStatus {
                name: "magick".into(),
                available: true,
                needed_for: String::new(),
            },
        ],
    )]);
    let offline = [(id(WA), 20)];
    let functions = ["media.convert".to_owned()];
    let unknown = [(
        "plugin_settings.misc.instagram.chats".to_owned(),
        ChatRef {
            connector_id: id(WA),
            address: ChatAddress::new("9@g.us", None),
        },
    )];
    let problems = evaluate(&Observed {
        tools: &tools,
        offline: &offline,
        unavailable_functions: &functions,
        unknown_chats: &unknown,
        ..Observed::default()
    });
    assert_eq!(
        keys(&problems),
        [
            "connector-offline:whatsapp-jio",
            "function-unavailable:media.convert",
            "tool-missing:mediaconv:ffmpeg",
            "unknown-chat:plugin_settings.misc.instagram.chats:whatsapp-jio:9@g.us",
        ]
    );
    assert!(problems.iter().all(|problem| problem.pause().is_none()));
    assert!(
        problems[2]
            .text
            .contains("animated stickers to WhatsApp do not work")
    );
}

#[test]
fn ignored_problems_do_not_pause() {
    let rules = [mirror(ChatFilter::All)];
    let problems = run(
        &rules,
        &facts(&[(
            TG,
            GROUP,
            ChatFacts {
                member: Fact::No,
                ..healthy()
            },
        )]),
    );
    assert_eq!(effective_pauses(&problems, &BTreeSet::new()).len(), 1);
    let ignored = BTreeSet::from([problems[0].key.clone()]);
    assert!(effective_pauses(&problems, &ignored).is_empty());
}
