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
    registry: &Arc<ConnectorCommands>,
    mut session: CommandSession,
    ok: bool,
    message: &'static str,
) -> tokio::task::JoinHandle<(CommandSession, pb::RunCommand)> {
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
    let registry = Arc::new(ConnectorCommands::default());
    assert!(registry.offered("wa").is_empty());
    let session = registry.attach("wa", declared(&["resync"]));
    assert_eq!(registry.offered("wa"), declared(&["resync"]));
    drop(session);
    assert!(registry.offered("wa").is_empty());
}

#[tokio::test]
async fn a_press_runs_on_the_session_and_returns_its_answer() {
    let registry = Arc::new(ConnectorCommands::default());
    let session = registry.attach("wa", declared(&["resync"]));
    let answered = answer(&registry, session, true, "resyncing");
    let outcome = registry.run("wa", "resync", COMMAND_TIMEOUT).await;
    assert_eq!(outcome, Ok("resyncing".into()));
    let (_session, command) = answered.await.unwrap();
    assert_eq!(command.command_id, "resync");
    assert!(!command.request_id.is_empty());
}

#[tokio::test]
async fn a_failed_command_is_an_error() {
    let registry = Arc::new(ConnectorCommands::default());
    let session = registry.attach("wa", declared(&["resync"]));
    let _answered = answer(&registry, session, false, "not linked");
    let outcome = registry.run("wa", "resync", COMMAND_TIMEOUT).await;
    assert_eq!(outcome, Err("not linked".into()));
}

#[tokio::test]
async fn a_disconnected_or_unknown_command_is_refused() {
    let registry = Arc::new(ConnectorCommands::default());
    assert_eq!(
        registry.run("wa", "resync", COMMAND_TIMEOUT).await,
        Err("wa is not connected".into())
    );
    let _session = registry.attach("wa", declared(&["resync"]));
    assert_eq!(
        registry.run("wa", "other", COMMAND_TIMEOUT).await,
        Err("wa does not offer \"other\"".into())
    );
}

#[tokio::test]
async fn an_unanswered_press_times_out() {
    let registry = Arc::new(ConnectorCommands::default());
    let _session = registry.attach("wa", declared(&["resync"]));
    let outcome = registry
        .run("wa", "resync", Duration::from_millis(50))
        .await;
    assert_eq!(outcome, Err("wa did not answer within 50ms".into()));
}

#[tokio::test]
async fn a_session_ending_mid_press_says_so() {
    let registry = Arc::new(ConnectorCommands::default());
    let mut session = registry.attach("wa", declared(&["resync"]));
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
    let registry = Arc::new(ConnectorCommands::default());
    let old = registry.attach("wa", declared(&["resync"]));
    let _new = registry.attach("wa", declared(&["resync", "fresh"]));
    drop(old);
    assert_eq!(registry.offered("wa"), declared(&["resync", "fresh"]));
}
