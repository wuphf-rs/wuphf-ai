//! Commands the owner runs on a connector from the core's settings panel
//! (plan 058), e.g. a resync: the connector declares them when it registers
//! ([`crate::Options::commands`]), the core offers them as buttons on the
//! connector's section while the session is live, and a press arrives as a
//! `RunCommand`, answered with a `CommandResult`. The core decides nothing
//! about them; the connector runs what it declared.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use wuphf_proto::connector as pb;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a connector can run for the owner. Answer quickly: the core waits a
/// few seconds for the result, so start slow work in the background and say
/// so.
pub trait Commands: Send + Sync + 'static {
    /// The commands to offer, in the order the panel lists them.
    fn declarations(&self) -> Vec<pb::ConnectorCommand>;

    /// Run the declared command `id`: `Ok` is what to tell the owner, `Err`
    /// why it failed.
    fn run<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<String, String>>;
}

/// The answer to `command`: a command the connector did not declare (or a
/// connector with none) is refused without running anything.
pub(crate) async fn answer(
    commands: Option<&Arc<dyn Commands>>,
    command: pb::RunCommand,
) -> pb::ClientFrame {
    let outcome = match commands {
        None => Err("this connector runs no commands".to_owned()),
        Some(commands)
            if !commands
                .declarations()
                .iter()
                .any(|declared| declared.id == command.command_id) =>
        {
            Err(format!("unknown command {:?}", command.command_id))
        }
        Some(commands) => commands.run(&command.command_id).await,
    };
    let (ok, message) = match outcome {
        Ok(message) => (true, message),
        Err(message) => (false, message),
    };
    pb::ClientFrame {
        body: Some(pb::client_frame::Body::CommandResult(pb::CommandResult {
            request_id: command.request_id,
            ok,
            message,
        })),
    }
}
