//! The commands live connector sessions declared (plan 058), run from the
//! settings panel: a press becomes a `RunCommand` on the connector's session,
//! and its `CommandResult` becomes the panel's notice. Commands are requests,
//! not dispatches: nothing is stored or redelivered, so a command for a
//! connector that is not connected is refused.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use wuphf_proto::connector as pb;

/// How long a press waits for the connector's answer.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Room for presses not yet written to a session.
const QUEUE: usize = 8;

/// The commands of every live connector session, by connector id.
#[derive(Default)]
pub struct ConnectorCommands {
    live: Mutex<HashMap<String, Live>>,
    sessions: AtomicU64,
}

/// One live session: what it declared, where its presses go, and the presses
/// awaiting an answer.
struct Live {
    session: u64,
    commands: Vec<pb::ConnectorCommand>,
    frames: mpsc::Sender<pb::ServerFrame>,
    pending: HashMap<String, oneshot::Sender<pb::CommandResult>>,
}

/// A session's side of the registry: its presses to write, and its detach
/// when dropped (at the session's end).
pub struct CommandSession {
    registry: Arc<ConnectorCommands>,
    connector: String,
    session: u64,
    /// The `RunCommand` frames to write to the connector.
    pub frames: mpsc::Receiver<pb::ServerFrame>,
}

impl ConnectorCommands {
    /// A connector's session went live, declaring `commands`; it replaces any
    /// earlier session of the connector.
    pub fn attach(
        self: &Arc<Self>,
        connector: &str,
        commands: Vec<pb::ConnectorCommand>,
    ) -> CommandSession {
        let session = self.sessions.fetch_add(1, Ordering::Relaxed);
        let (frames, receiver) = mpsc::channel(QUEUE);
        self.live.lock().unwrap().insert(
            connector.to_owned(),
            Live {
                session,
                commands,
                frames,
                pending: HashMap::new(),
            },
        );
        CommandSession {
            registry: Arc::clone(self),
            connector: connector.to_owned(),
            session,
            frames: receiver,
        }
    }

    /// The commands `connector` offers now: none unless it is connected.
    pub fn offered(&self, connector: &str) -> Vec<pb::ConnectorCommand> {
        self.live
            .lock()
            .unwrap()
            .get(connector)
            .map(|live| live.commands.clone())
            .unwrap_or_default()
    }

    /// Run `command` on `connector`'s session: its answer's message, or why
    /// there is none.
    pub async fn run(
        &self,
        connector: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let request_id = Uuid::new_v4().to_string();
        let (answer, answered) = oneshot::channel();
        let frames = {
            let mut live = self.live.lock().unwrap();
            let Some(session) = live.get_mut(connector) else {
                return Err(format!("{connector} is not connected"));
            };
            if !session.commands.iter().any(|offered| offered.id == command) {
                return Err(format!("{connector} does not offer {command:?}"));
            }
            session.pending.insert(request_id.clone(), answer);
            session.frames.clone()
        };
        let frame = pb::ServerFrame {
            body: Some(pb::server_frame::Body::RunCommand(pb::RunCommand {
                request_id: request_id.clone(),
                command_id: command.to_owned(),
            })),
        };
        if frames.try_send(frame).is_err() {
            self.forget(connector, &request_id);
            return Err(format!("{connector} is busy; try again"));
        }
        match tokio::time::timeout(timeout, answered).await {
            Ok(Ok(result)) if result.ok => Ok(result.message),
            Ok(Ok(result)) => Err(result.message),
            // The session ended before answering.
            Ok(Err(_)) => Err(format!("{connector} disconnected before answering")),
            Err(_) => {
                self.forget(connector, &request_id);
                Err(format!("{connector} did not answer within {timeout:?}"))
            }
        }
    }

    /// A session's answer; one nobody waits for any more is dropped.
    pub fn settle(&self, connector: &str, result: pb::CommandResult) {
        let waiting = self
            .live
            .lock()
            .unwrap()
            .get_mut(connector)
            .and_then(|live| live.pending.remove(&result.request_id));
        if let Some(waiting) = waiting {
            let _ = waiting.send(result);
        }
    }

    fn forget(&self, connector: &str, request_id: &str) {
        if let Some(live) = self.live.lock().unwrap().get_mut(connector) {
            live.pending.remove(request_id);
        }
    }
}

impl Drop for CommandSession {
    /// Detach the session, unless a newer one of the connector replaced it.
    fn drop(&mut self) {
        let mut live = self.registry.live.lock().unwrap();
        if live
            .get(&self.connector)
            .is_some_and(|live| live.session == self.session)
        {
            live.remove(&self.connector);
        }
    }
}

#[cfg(test)]
mod tests;
