//! The live connector sessions, and the requests the core makes on them that
//! are not dispatches: the commands a session declared (plan 058), run from
//! the settings panel (`RunCommand` → `CommandResult`), and health probes
//! (plan 079, `ProbeChats` → `ProbeResult`). Nothing here is stored or
//! redelivered, so a request to a connector that is not connected is refused.
//!
//! The registry also says when sessions come and go ([`SessionEvent`]), and
//! keeps the tools each session reported, for the health checks.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;
use wuphf_model::ToolStatus;
use wuphf_proto::connector as pb;

/// How long a press waits for the connector's answer.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a probe waits for the connector's answer; a later one is unknown.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Room for requests not yet written to a session.
const QUEUE: usize = 8;

/// A session came or went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Attached(String),
    Detached(String),
}

/// Every live connector session, by connector id.
pub struct LiveSessions {
    live: Mutex<HashMap<String, Live>>,
    sessions: AtomicU64,
    events: broadcast::Sender<SessionEvent>,
}

impl Default for LiveSessions {
    fn default() -> Self {
        Self {
            live: Mutex::default(),
            sessions: AtomicU64::default(),
            events: broadcast::channel(64).0,
        }
    }
}

/// One live session: what it declared, where its requests go, and the
/// requests awaiting an answer.
struct Live {
    session: u64,
    commands: Vec<pb::ConnectorCommand>,
    tools: Vec<ToolStatus>,
    frames: mpsc::Sender<pb::ServerFrame>,
    pending: HashMap<String, oneshot::Sender<pb::CommandResult>>,
    probes: HashMap<String, oneshot::Sender<pb::ProbeResult>>,
}

/// A session's side of the registry: its requests to write, and its detach
/// when dropped (at the session's end).
pub struct LiveSession {
    registry: Arc<LiveSessions>,
    connector: String,
    session: u64,
    /// The `RunCommand` and `ProbeChats` frames to write to the connector.
    pub frames: mpsc::Receiver<pb::ServerFrame>,
}

impl LiveSessions {
    /// A connector's session went live, declaring `commands` and reporting
    /// `tools`; it replaces any earlier session of the connector.
    pub fn attach(
        self: &Arc<Self>,
        connector: &str,
        commands: Vec<pb::ConnectorCommand>,
        tools: Vec<pb::ToolStatus>,
    ) -> LiveSession {
        let session = self.sessions.fetch_add(1, Ordering::Relaxed);
        let (frames, receiver) = mpsc::channel(QUEUE);
        self.live.lock().unwrap().insert(
            connector.to_owned(),
            Live {
                session,
                commands,
                tools: tools.into_iter().map(tool_from_wire).collect(),
                frames,
                pending: HashMap::new(),
                probes: HashMap::new(),
            },
        );
        let _ = self
            .events
            .send(SessionEvent::Attached(connector.to_owned()));
        LiveSession {
            registry: Arc::clone(self),
            connector: connector.to_owned(),
            session,
            frames: receiver,
        }
    }

    /// Sessions coming and going, from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// The connectors with a live session.
    pub fn connected(&self) -> Vec<String> {
        let mut connected: Vec<_> = self.live.lock().unwrap().keys().cloned().collect();
        connected.sort();
        connected
    }

    /// The tools each live session reported last, by connector id.
    pub fn tools(&self) -> BTreeMap<String, Vec<ToolStatus>> {
        self.live
            .lock()
            .unwrap()
            .iter()
            .map(|(connector, live)| (connector.clone(), live.tools.clone()))
            .collect()
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

    /// Ask `connector` about `chat_ids`: its answer, or why there is none
    /// (then every chat is unknown). The answer's tools replace those the
    /// session reported before.
    pub async fn probe(
        &self,
        connector: &str,
        chat_ids: Vec<String>,
        timeout: Duration,
    ) -> Result<pb::ProbeResult, String> {
        let request_id = Uuid::new_v4().to_string();
        let (answer, answered) = oneshot::channel();
        let frames = {
            let mut live = self.live.lock().unwrap();
            let Some(session) = live.get_mut(connector) else {
                return Err(format!("{connector} is not connected"));
            };
            session.probes.insert(request_id.clone(), answer);
            session.frames.clone()
        };
        let frame = pb::ServerFrame {
            body: Some(pb::server_frame::Body::Probe(pb::ProbeChats {
                request_id: request_id.clone(),
                chat_ids,
            })),
        };
        if frames.try_send(frame).is_err() {
            self.forget(connector, &request_id);
            return Err(format!("{connector} is busy"));
        }
        match tokio::time::timeout(timeout, answered).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(format!("{connector} disconnected before answering")),
            Err(_) => {
                self.forget(connector, &request_id);
                Err(format!("{connector} did not answer within {timeout:?}"))
            }
        }
    }

    /// A session's answer to a command; one nobody waits for any more is
    /// dropped.
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

    /// A session's answer to a probe. Its tools are kept even when nobody
    /// waits for the answer any more.
    pub fn settle_probe(&self, connector: &str, result: pb::ProbeResult) {
        let waiting = {
            let mut live = self.live.lock().unwrap();
            let Some(live) = live.get_mut(connector) else {
                return;
            };
            live.tools = result.tools.iter().cloned().map(tool_from_wire).collect();
            live.probes.remove(&result.request_id)
        };
        if let Some(waiting) = waiting {
            let _ = waiting.send(result);
        }
    }

    fn forget(&self, connector: &str, request_id: &str) {
        if let Some(live) = self.live.lock().unwrap().get_mut(connector) {
            live.pending.remove(request_id);
            live.probes.remove(request_id);
        }
    }
}

impl Drop for LiveSession {
    /// Detach the session, unless a newer one of the connector replaced it.
    fn drop(&mut self) {
        let mut live = self.registry.live.lock().unwrap();
        if live
            .get(&self.connector)
            .is_some_and(|live| live.session == self.session)
        {
            live.remove(&self.connector);
            let _ = self
                .registry
                .events
                .send(SessionEvent::Detached(self.connector.clone()));
        }
    }
}

/// A tool as a session reported it.
pub fn tool_from_wire(tool: pb::ToolStatus) -> ToolStatus {
    ToolStatus {
        name: tool.name,
        available: tool.available,
        needed_for: tool.needed_for,
    }
}

#[cfg(test)]
mod tests;
