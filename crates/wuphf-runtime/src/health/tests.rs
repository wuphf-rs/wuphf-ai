use super::*;
use crate::live_sessions::LiveSession;
use crate::{Observer, owner_send::OwnerMessageSender};
use std::path::PathBuf;
use tokio::sync::mpsc;
use wuphf_core::Core;
use wuphf_model::{Action, Dispatch};
use wuphf_settings::{LiveRouting, WuphfSchema, base, load};
use wuphf_storage::{HealthStore, InMemoryStorage};

const TG: &str = "telegram-main";
const GROUP: &str = "telegram:channel:100";

const CONFIG: &str = r#"
listen = "127.0.0.1:50051"
database_url = "sqlite://unused.sqlite"
notify = ["telegram-main:telegram:user:1"]

[[connectors]]
id = "telegram-main"
platform = "telegram"
token_env = "TG"
supports_threads = true
supports_topic_create = true

[[connectors]]
id = "whatsapp-main"
platform = "whatsapp"
token_env = "WA"

[rules.mirror]
kind = "many-to-one"
source_connector = "whatsapp-main"
destination = "telegram-main:telegram:channel:100"
"#;

fn config_file() -> PathBuf {
    let path = std::env::temp_dir().join(format!("wuphf-health-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&path, CONFIG).unwrap();
    path
}

pub(crate) struct Fixture {
    pub(crate) storage: Arc<InMemoryStorage>,
    core: Arc<Core<InMemoryStorage>>,
    sessions: Arc<LiveSessions>,
    pub(crate) monitor: Arc<HealthMonitor<InMemoryStorage>>,
    sent: mpsc::Receiver<Dispatch>,
}

async fn fixture_over(storage: Arc<InMemoryStorage>) -> Fixture {
    let schema = WuphfSchema::with_env(|name| Some(format!("secret-{name}")));
    let settings = Arc::new(
        load(
            schema,
            base(&config_file()).unwrap(),
            Arc::new(botconf::MemoryStorage::new()),
        )
        .await
        .unwrap(),
    );
    let core = Arc::new(Core::new(
        Arc::clone(&storage),
        LiveRouting::new(Arc::clone(&settings)),
    ));
    let (tx, sent) = mpsc::channel(64);
    let sender = OwnerMessageSender::new(
        Arc::clone(&core),
        Arc::new(Observer::new(Arc::clone(&storage))),
        tx,
    );
    let sessions = Arc::new(LiveSessions::default());
    let monitor = HealthMonitor::new(
        Arc::clone(&storage),
        Arc::clone(&sessions),
        sender,
        settings,
        None,
    );
    Fixture {
        storage,
        core,
        sessions,
        monitor,
        sent,
    }
}

pub(crate) async fn fixture() -> Fixture {
    fixture_over(Arc::new(InMemoryStorage::default())).await
}

/// A Telegram session that answers every probe with `facts` for the group.
fn telegram(
    sessions: &Arc<LiveSessions>,
    facts: Arc<Mutex<pb::ChatFacts>>,
) -> tokio::task::JoinHandle<LiveSession> {
    let mut session = sessions.attach(TG, Vec::new(), Vec::new());
    let sessions = Arc::clone(sessions);
    tokio::spawn(async move {
        while let Some(frame) = session.frames.recv().await {
            if let Some(pb::server_frame::Body::Probe(probe)) = frame.body {
                let chats = probe
                    .chat_ids
                    .iter()
                    .map(|chat_id| pb::ChatFacts {
                        chat_id: chat_id.clone(),
                        ..facts.lock().unwrap().clone()
                    })
                    .collect();
                sessions.settle_probe(
                    TG,
                    pb::ProbeResult {
                        request_id: probe.request_id,
                        chats,
                        tools: Vec::new(),
                    },
                );
            }
        }
        session
    })
}

fn facts(forum: pb::Fact) -> pb::ChatFacts {
    pb::ChatFacts {
        exists: pb::Fact::Yes.into(),
        member: pb::Fact::Yes.into(),
        can_send: pb::Fact::Yes.into(),
        forum: forum.into(),
        can_create_topics: pb::Fact::Yes.into(),
        ..Default::default()
    }
}

fn forward_pause() -> HealthPause {
    HealthPause {
        rule: wuphf_model::RuleId("mirror".into()),
        direction: FlowDirection::Forward,
        connector: ConnectorId(TG.into()),
        chat_id: GROUP.into(),
    }
}

/// The texts of the notices sent so far.
fn notices(sent: &mut mpsc::Receiver<Dispatch>) -> Vec<String> {
    let mut texts = Vec::new();
    while let Ok(dispatch) = sent.try_recv() {
        if let Action::Send { message, .. } = dispatch.action {
            texts.extend(message.text);
        }
    }
    texts
}

#[tokio::test]
async fn a_group_without_topics_pauses_the_rule_and_says_so_once() {
    let mut fixture = fixture().await;
    let answer = Arc::new(Mutex::new(facts(pb::Fact::No)));
    let _telegram = telegram(&fixture.sessions, Arc::clone(&answer));

    let report = fixture.monitor.run().await.unwrap();
    let keys: Vec<_> = report
        .problems
        .iter()
        .map(|stored| stored.problem.key.as_str())
        .collect();
    assert_eq!(
        keys,
        ["target-not-forum:mirror:telegram-main:telegram:channel:100"]
    );
    assert_eq!(
        *fixture.core.health_pauses(),
        BTreeSet::from([forward_pause()])
    );
    let told = notices(&mut fixture.sent);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].starts_with("⚠️ Rule mirror:"), "{}", told[0]);
    assert!(told[0].contains("held until it is fixed"), "{}", told[0]);

    // Checking again says nothing new.
    fixture.monitor.run().await.unwrap();
    assert!(notices(&mut fixture.sent).is_empty());

    // Fixed: the pause ends, with a ✅.
    *answer.lock().unwrap() = facts(pb::Fact::Yes);
    let report = fixture.monitor.run().await.unwrap();
    assert!(report.problems.is_empty());
    assert!(fixture.core.health_pauses().is_empty());
    let told = notices(&mut fixture.sent);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].starts_with("✅ Fixed:"), "{}", told[0]);
    assert!(told[0].contains("0 held messages sent"), "{}", told[0]);
}

#[tokio::test]
async fn an_unknown_answer_neither_pauses_nor_resumes() {
    let fixture = fixture().await;
    let answer = Arc::new(Mutex::new(facts(pb::Fact::No)));
    let _telegram = telegram(&fixture.sessions, Arc::clone(&answer));
    fixture.monitor.run().await.unwrap();

    // The connector no longer knows: the last known fact stands.
    *answer.lock().unwrap() = pb::ChatFacts::default();
    let report = fixture.monitor.run().await.unwrap();
    assert_eq!(report.problems.len(), 1);
    assert_eq!(fixture.core.health_pauses().len(), 1);
}

#[tokio::test]
async fn a_restart_keeps_what_it_could_not_check_again() {
    let storage = Arc::new(InMemoryStorage::default());
    let first = fixture_over(Arc::clone(&storage)).await;
    let _telegram = telegram(&first.sessions, Arc::new(Mutex::new(facts(pb::Fact::No))));
    first.monitor.run().await.unwrap();

    // A new monitor over the same storage, with Telegram not connected yet:
    // the pause is restored and the problem stands.
    let second = fixture_over(storage).await;
    second.monitor.restore().await.unwrap();
    assert_eq!(second.core.health_pauses().len(), 1);
    let report = second.monitor.run().await.unwrap();
    assert_eq!(report.problems.len(), 1);
    assert_eq!(second.core.health_pauses().len(), 1);
}

#[tokio::test]
async fn ignoring_resumes_and_lapses_once_the_problem_passes() {
    let mut fixture = fixture().await;
    let answer = Arc::new(Mutex::new(facts(pb::Fact::No)));
    let _telegram = telegram(&fixture.sessions, Arc::clone(&answer));
    let report = fixture.monitor.run().await.unwrap();
    let key = report.problems[0].problem.key.clone();
    notices(&mut fixture.sent);

    let report = fixture.monitor.ignore(&key, true).await.unwrap();
    assert!(report.ignored.contains(&key));
    assert!(fixture.core.health_pauses().is_empty());
    // An ignored problem clearing says nothing, and the ignore lapses.
    *answer.lock().unwrap() = facts(pb::Fact::Yes);
    let report = fixture.monitor.run().await.unwrap();
    assert!(report.problems.is_empty());
    assert!(report.ignored.is_empty());
    assert!(notices(&mut fixture.sent).is_empty());
    assert!(fixture.storage.health_ignores().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_connector_away_for_long_is_a_warning() {
    let fixture = fixture().await;
    // Nothing connected, and the monitor started long ago.
    let monitor = HealthMonitor {
        started_ms: now_ms() - OFFLINE_AFTER_MS - 60_000,
        ..Arc::try_unwrap(fixture.monitor).ok().unwrap()
    };
    let report = monitor.run().await.unwrap();
    let keys: Vec<_> = report
        .problems
        .iter()
        .map(|stored| stored.problem.key.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "connector-offline:telegram-main",
            "connector-offline:whatsapp-main"
        ]
    );
    assert!(
        report
            .problems
            .iter()
            .all(|stored| stored.problem.pause().is_none())
    );
}

#[tokio::test]
async fn a_session_that_just_ended_is_not_away_since_startup() {
    // The monitor started long ago, and telegram-main's session ends now.
    // Checked before its Detached event is handled, it is away since its
    // session ended, not since startup.
    let fixture = fixture().await;
    let sessions = Arc::clone(&fixture.sessions);
    let monitor = HealthMonitor {
        started_ms: now_ms() - OFFLINE_AFTER_MS - 60_000,
        ..Arc::try_unwrap(fixture.monitor).ok().unwrap()
    };
    drop(sessions.attach(TG, Vec::new(), Vec::new()));
    let report = monitor.run().await.unwrap();
    let keys: Vec<_> = report
        .problems
        .iter()
        .map(|stored| stored.problem.key.as_str())
        .collect();
    assert_eq!(keys, ["connector-offline:whatsapp-main"]);
}
