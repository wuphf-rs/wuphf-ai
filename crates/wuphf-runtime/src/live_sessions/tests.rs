use super::*;

fn declared(ids: &[&str]) -> Vec<pb::ConnectorCommand> {
    ids.iter()
        .map(|id| pb::ConnectorCommand {
            id: (*id).into(),
            title: format!("Do {id}"),
        })
        .collect()
}

/// Answers the next press on `session` with `ok` and `message`.
fn answer(
    registry: &Arc<LiveSessions>,
    mut session: LiveSession,
    ok: bool,
    message: &'static str,
) -> tokio::task::JoinHandle<(LiveSession, pb::RunCommand)> {
    let registry = Arc::clone(registry);
    tokio::spawn(async move {
        let frame = session.frames.recv().await.unwrap();
        let Some(pb::server_frame::Body::RunCommand(command)) = frame.body else {
            panic!("a RunCommand");
        };
        registry.settle(
            "wa",
            pb::CommandResult {
                request_id: command.request_id.clone(),
                ok,
                message: message.into(),
            },
        );
        (session, command)
    })
}

#[tokio::test]
async fn commands_are_offered_only_while_the_session_is_live() {
    let registry = Arc::new(LiveSessions::default());
    assert!(registry.offered("wa").is_empty());
    let session = registry.attach("wa", declared(&["resync"]), Vec::new());
    assert_eq!(registry.offered("wa"), declared(&["resync"]));
    drop(session);
    assert!(registry.offered("wa").is_empty());
}

#[tokio::test]
async fn a_press_runs_on_the_session_and_returns_its_answer() {
    let registry = Arc::new(LiveSessions::default());
    let session = registry.attach("wa", declared(&["resync"]), Vec::new());
    let answered = answer(&registry, session, true, "resyncing");
    let outcome = registry.run("wa", "resync", COMMAND_TIMEOUT).await;
    assert_eq!(outcome, Ok("resyncing".into()));
    let (_session, command) = answered.await.unwrap();
    assert_eq!(command.command_id, "resync");
    assert!(!command.request_id.is_empty());
}

#[tokio::test]
async fn a_failed_command_is_an_error() {
    let registry = Arc::new(LiveSessions::default());
    let session = registry.attach("wa", declared(&["resync"]), Vec::new());
    let _answered = answer(&registry, session, false, "not linked");
    let outcome = registry.run("wa", "resync", COMMAND_TIMEOUT).await;
    assert_eq!(outcome, Err("not linked".into()));
}

#[tokio::test]
async fn a_disconnected_or_unknown_command_is_refused() {
    let registry = Arc::new(LiveSessions::default());
    assert_eq!(
        registry.run("wa", "resync", COMMAND_TIMEOUT).await,
        Err("wa is not connected".into())
    );
    let _session = registry.attach("wa", declared(&["resync"]), Vec::new());
    assert_eq!(
        registry.run("wa", "other", COMMAND_TIMEOUT).await,
        Err("wa does not offer \"other\"".into())
    );
}

#[tokio::test]
async fn an_unanswered_press_times_out() {
    let registry = Arc::new(LiveSessions::default());
    let _session = registry.attach("wa", declared(&["resync"]), Vec::new());
    let outcome = registry
        .run("wa", "resync", Duration::from_millis(50))
        .await;
    assert_eq!(outcome, Err("wa did not answer within 50ms".into()));
}

#[tokio::test]
async fn a_session_ending_mid_press_says_so() {
    let registry = Arc::new(LiveSessions::default());
    let mut session = registry.attach("wa", declared(&["resync"]), Vec::new());
    let ended = tokio::spawn(async move {
        session.frames.recv().await.unwrap();
        drop(session);
    });
    let outcome = registry.run("wa", "resync", COMMAND_TIMEOUT).await;
    ended.await.unwrap();
    assert_eq!(outcome, Err("wa disconnected before answering".into()));
}

#[tokio::test]
async fn an_old_session_ending_keeps_the_new_one() {
    let registry = Arc::new(LiveSessions::default());
    let old = registry.attach("wa", declared(&["resync"]), Vec::new());
    let _new = registry.attach("wa", declared(&["resync", "fresh"]), Vec::new());
    drop(old);
    assert_eq!(registry.offered("wa"), declared(&["resync", "fresh"]));
}

fn tool(name: &str, available: bool) -> pb::ToolStatus {
    pb::ToolStatus {
        name: name.into(),
        available,
        needed_for: "sticker pack names".into(),
    }
}

#[tokio::test]
async fn a_probe_returns_the_facts_and_refreshes_the_tools() {
    let registry = Arc::new(LiveSessions::default());
    let mut session = registry.attach("wa", Vec::new(), vec![tool("webpmux", false)]);
    assert!(!registry.tools()["wa"][0].available);
    let answering = Arc::clone(&registry);
    let answered = tokio::spawn(async move {
        let frame = session.frames.recv().await.unwrap();
        let Some(pb::server_frame::Body::Probe(probe)) = frame.body else {
            panic!("a ProbeChats");
        };
        assert_eq!(probe.chat_ids, ["1@g.us"]);
        answering.settle_probe(
            "wa",
            pb::ProbeResult {
                request_id: probe.request_id,
                chats: vec![pb::ChatFacts {
                    chat_id: "1@g.us".into(),
                    member: pb::Fact::No.into(),
                    ..Default::default()
                }],
                tools: vec![tool("webpmux", true)],
            },
        );
        session
    });
    let result = registry
        .probe("wa", vec!["1@g.us".into()], PROBE_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(result.chats[0].member(), pb::Fact::No);
    assert!(registry.tools()["wa"][0].available);
    let _session = answered.await.unwrap();
}

#[tokio::test]
async fn sessions_coming_and_going_are_announced() {
    let registry = Arc::new(LiveSessions::default());
    let mut events = registry.subscribe();
    let session = registry.attach("wa", Vec::new(), Vec::new());
    assert_eq!(registry.connected(), ["wa"]);
    drop(session);
    assert!(registry.connected().is_empty());
    assert_eq!(
        events.recv().await.unwrap(),
        SessionEvent::Attached("wa".into())
    );
    assert_eq!(
        events.recv().await.unwrap(),
        SessionEvent::Detached("wa".into())
    );
    // An unanswered probe of a gone connector is refused at once.
    assert!(
        registry
            .probe("wa", Vec::new(), PROBE_TIMEOUT)
            .await
            .unwrap_err()
            .contains("not connected")
    );
}
