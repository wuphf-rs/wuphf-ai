//! The function hub: one task owning every provider session, function owner,
//! call queue and in-flight call. Modeled on the connector hub: services talk
//! to it through a cloneable [`FunctionHubHandle`] that sends commands over a
//! channel, so no state is shared behind locks and every transition happens in
//! one place.
//!
//! A call is queued under its function key, handed to the owning provider while
//! the owner has a free concurrency slot, and answered when the provider's
//! result arrives. A caller that gives up abandons the call, which frees its
//! slot and cancels it at the provider. A provider session that ends re-queues
//! its idempotent in-flight calls and fails the rest; queued calls keep waiting
//! for the next provider.

use crate::config::{FunctionKey, Functions};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use wuphf_proto::connector as pb;

/// Upper bound on what a provider may declare as a function's concurrency.
const MAX_DECLARED_CONCURRENCY: u32 = 1024;

pub(crate) type CallId = String;

/// Proves which provider session sent a frame. A frame from a session that has
/// since been replaced carries a stale `seq` and is ignored.
#[derive(Debug, Clone)]
pub(crate) struct SessionTicket {
    pub provider_id: String,
    pub seq: u64,
}

pub(crate) struct Call {
    pub id: CallId,
    pub key: FunctionKey,
    pub args: Vec<u8>,
    pub media_ids: Vec<String>,
    pub caller: String,
    /// The caller's own deadline, from its `grpc-timeout` header.
    pub requested: Option<Duration>,
}

pub(crate) enum CallOutcome {
    Completed {
        result: Vec<u8>,
        media_ids: Vec<String>,
        provider_id: String,
        queued_for: Duration,
    },
    Failed(CallFailure),
}

pub(crate) enum CallFailure {
    Provider {
        code: pb::CallErrorCode,
        message: String,
        provider_id: String,
    },
    ProviderLost {
        provider_id: String,
    },
}

#[derive(Debug)]
pub(crate) enum InvokeRejection {
    NotServable,
    QueueFull { limit: usize },
    Stopped,
}

#[derive(Debug)]
pub(crate) enum RegisterRejection {
    /// A malformed registration: no functions, a bad name or major, a key
    /// declared twice, or a concurrency above the limit.
    Invalid(String),
    /// A function outside the provider's configured patterns.
    NotPermitted {
        function: String,
    },
    /// A function another provider id serves.
    Owned {
        function: String,
        owner: String,
    },
    Stopped,
}

/// Where an abandoned call was when the caller gave up.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AbandonReport {
    Queued { provider_connected: bool },
    InFlight { provider_id: String },
    Unknown,
}

#[derive(Clone)]
pub struct FunctionHubHandle {
    commands: mpsc::Sender<Command>,
    /// Unbounded so [`abandon`](Self::abandon) never blocks; it runs from `Drop`.
    abandons: mpsc::UnboundedSender<CallId>,
}

enum Command {
    Register {
        provider_id: String,
        functions: Vec<pb::FunctionDeclaration>,
        output: mpsc::Sender<pb::ProviderServerFrame>,
        reply: oneshot::Sender<Result<SessionTicket, RegisterRejection>>,
    },
    Invoke {
        call: Call,
        reply: oneshot::Sender<InvokeReply>,
    },
    Result {
        ticket: SessionTicket,
        result: pb::CallResult,
    },
    Closed {
        ticket: SessionTicket,
    },
    Abandon {
        id: CallId,
        reply: oneshot::Sender<AbandonReport>,
    },
    List {
        reply: oneshot::Sender<Vec<pb::FunctionStatus>>,
    },
}

type InvokeReply = Result<(Duration, oneshot::Receiver<CallOutcome>), InvokeRejection>;

impl FunctionHubHandle {
    /// Register a provider session. `output` carries calls and cancels to the
    /// provider; the hub drops it when the session ends or is replaced.
    pub(crate) async fn register(
        &self,
        provider_id: String,
        functions: Vec<pb::FunctionDeclaration>,
        output: mpsc::Sender<pb::ProviderServerFrame>,
    ) -> Result<SessionTicket, RegisterRejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::Register {
            provider_id,
            functions,
            output,
            reply,
        })
        .await;
        receiver.await.unwrap_or(Err(RegisterRejection::Stopped))
    }

    /// Queue a call. Returns its deadline and a receiver for its outcome.
    pub(crate) async fn invoke(&self, call: Call) -> InvokeReply {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::Invoke { call, reply }).await;
        receiver.await.unwrap_or(Err(InvokeRejection::Stopped))
    }

    pub(crate) async fn result(&self, ticket: SessionTicket, result: pb::CallResult) {
        self.send(Command::Result { ticket, result }).await;
    }

    /// The session behind `ticket` ended.
    pub(crate) async fn closed(&self, ticket: SessionTicket) {
        self.send(Command::Closed { ticket }).await;
    }

    /// Give up on a call without waiting. Safe to call from `Drop`.
    pub(crate) fn abandon(&self, id: CallId) {
        let _ = self.abandons.send(id);
    }

    /// Give up on a call and report where it was.
    pub(crate) async fn abandon_and_report(&self, id: CallId) -> AbandonReport {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::Abandon { id, reply }).await;
        receiver.await.unwrap_or(AbandonReport::Unknown)
    }

    pub(crate) async fn list(&self) -> Vec<pb::FunctionStatus> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::List { reply }).await;
        receiver.await.unwrap_or_default()
    }

    /// A stopped hub drops the command, which its caller observes as a closed
    /// reply channel.
    async fn send(&self, command: Command) {
        let _ = self.commands.send(command).await;
    }
}

pub struct FunctionHub {
    functions: Arc<Functions>,
    commands: mpsc::Receiver<Command>,
    abandons: mpsc::UnboundedReceiver<CallId>,
    sessions: HashMap<String, Session>,
    owners: HashMap<FunctionKey, Owner>,
    queues: HashMap<FunctionKey, VecDeque<Queued>>,
    running: HashMap<CallId, Running>,
    stats: HashMap<FunctionKey, Stats>,
    next_seq: u64,
}

struct Session {
    seq: u64,
    output: mpsc::Sender<pb::ProviderServerFrame>,
    keys: Vec<FunctionKey>,
}

struct Owner {
    provider_id: String,
    seq: u64,
    declaration: pb::FunctionDeclaration,
    in_flight: u32,
    limit: u32,
}

struct Queued {
    call: Call,
    deadline_unix_ms: i64,
    enqueued_at: Instant,
    reply: oneshot::Sender<CallOutcome>,
}

struct Running {
    queued: Queued,
    provider_id: String,
    seq: u64,
    started_at: Instant,
}

#[derive(Default)]
struct Stats {
    completed: u64,
    failed: u64,
    abandoned: u64,
}

impl FunctionHub {
    pub fn new(functions: Arc<Functions>) -> (Self, FunctionHubHandle) {
        let (commands_tx, commands) = mpsc::channel(256);
        let (abandons_tx, abandons) = mpsc::unbounded_channel();
        (
            Self {
                functions,
                commands,
                abandons,
                sessions: HashMap::new(),
                owners: HashMap::new(),
                queues: HashMap::new(),
                running: HashMap::new(),
                stats: HashMap::new(),
                next_seq: 0,
            },
            FunctionHubHandle {
                commands: commands_tx,
                abandons: abandons_tx,
            },
        )
    }

    /// Serve commands until every handle is dropped.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(command) => self.handle(command),
                    None => break,
                },
                Some(id) = self.abandons.recv() => {
                    self.abandon(&id);
                }
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Register {
                provider_id,
                functions,
                output,
                reply,
            } => {
                let _ = reply.send(self.register(provider_id, functions, output));
            }
            Command::Invoke { call, reply } => {
                let _ = reply.send(self.invoke(call));
            }
            Command::Result { ticket, result } => self.result(&ticket, result),
            Command::Closed { ticket } => {
                if self
                    .sessions
                    .get(&ticket.provider_id)
                    .is_some_and(|session| session.seq == ticket.seq)
                {
                    self.drop_session(&ticket.provider_id);
                }
            }
            Command::Abandon { id, reply } => {
                let _ = reply.send(self.abandon(&id));
            }
            Command::List { reply } => {
                let _ = reply.send(self.list());
            }
        }
    }

    fn register(
        &mut self,
        provider_id: String,
        declarations: Vec<pb::FunctionDeclaration>,
        output: mpsc::Sender<pb::ProviderServerFrame>,
    ) -> Result<SessionTicket, RegisterRejection> {
        let keys = self.validate(&provider_id, &declarations)?;
        if let Some((key, owner)) = keys.iter().find_map(|key| {
            self.owners
                .get(key)
                .filter(|owner| owner.provider_id != provider_id)
                .map(|owner| (key, owner))
        }) {
            return Err(RegisterRejection::Owned {
                function: key.to_string(),
                owner: owner.provider_id.clone(),
            });
        }
        if self.sessions.contains_key(&provider_id) {
            self.drop_session(&provider_id);
            info!(provider = %provider_id, "provider session replaced by a new session");
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        for (key, declaration) in keys.iter().zip(declarations) {
            let limit = self
                .functions
                .max_concurrency(&key.name, declaration.max_concurrency);
            self.owners.insert(
                key.clone(),
                Owner {
                    provider_id: provider_id.clone(),
                    seq,
                    declaration,
                    in_flight: 0,
                    limit,
                },
            );
        }
        let listed = keys
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        self.sessions.insert(
            provider_id.clone(),
            Session {
                seq,
                output,
                keys: keys.clone(),
            },
        );
        info!(provider = %provider_id, functions = %listed, "provider session registered");
        for key in &keys {
            self.drain(key);
        }
        Ok(SessionTicket { provider_id, seq })
    }

    /// Check a whole registration; any bad declaration rejects all of it.
    fn validate(
        &self,
        provider_id: &str,
        declarations: &[pb::FunctionDeclaration],
    ) -> Result<Vec<FunctionKey>, RegisterRejection> {
        if declarations.is_empty() {
            return Err(RegisterRejection::Invalid(
                "a provider must declare at least one function".into(),
            ));
        }
        let mut keys = Vec::with_capacity(declarations.len());
        for declaration in declarations {
            let key = FunctionKey::from_proto(declaration.function.clone())
                .map_err(RegisterRejection::Invalid)?;
            if keys.contains(&key) {
                return Err(RegisterRejection::Invalid(format!(
                    "{key} is declared more than once"
                )));
            }
            if declaration.max_concurrency > MAX_DECLARED_CONCURRENCY {
                return Err(RegisterRejection::Invalid(format!(
                    "{key} declares max_concurrency {}; the limit is {MAX_DECLARED_CONCURRENCY}",
                    declaration.max_concurrency
                )));
            }
            if !self.functions.may_register(provider_id, &key.name) {
                return Err(RegisterRejection::NotPermitted {
                    function: key.to_string(),
                });
            }
            keys.push(key);
        }
        Ok(keys)
    }

    fn invoke(&mut self, call: Call) -> InvokeReply {
        let name = call.key.name.as_str();
        if !self.functions.is_servable(name) {
            return Err(InvokeRejection::NotServable);
        }
        let suggested = self
            .owners
            .get(&call.key)
            .and_then(|owner| owner.declaration.default_deadline_ms)
            .map(|ms| Duration::from_millis(ms.into()));
        let deadline = self.functions.deadline(name, call.requested, suggested);
        let limit = self.functions.max_queued_calls(name);
        let queue = self.queues.entry(call.key.clone()).or_default();
        let before = queue.len();
        queue.retain(|queued| !queued.reply.is_closed());
        let pruned = before - queue.len();
        if pruned > 0 {
            self.stats.entry(call.key.clone()).or_default().abandoned += pruned as u64;
        }
        let queue = self.queues.entry(call.key.clone()).or_default();
        if queue.len() >= limit {
            return Err(InvokeRejection::QueueFull { limit });
        }

        let (reply, receiver) = oneshot::channel();
        let key = call.key.clone();
        queue.push_back(Queued {
            call,
            deadline_unix_ms: unix_ms(SystemTime::now() + deadline),
            enqueued_at: Instant::now(),
            reply,
        });
        self.drain(&key);
        Ok((deadline, receiver))
    }

    fn result(&mut self, ticket: &SessionTicket, result: pb::CallResult) {
        let current = self.running.get(&result.call_id).is_some_and(|running| {
            running.provider_id == ticket.provider_id && running.seq == ticket.seq
        });
        if !current {
            debug!(
                provider = %ticket.provider_id,
                call = %result.call_id,
                "ignoring a result for a call that is no longer running there",
            );
            return;
        }
        let running = self.running.remove(&result.call_id).expect("checked above");
        let key = running.queued.call.key.clone();
        self.release_slot(&key, running.seq);

        let provider_id = running.provider_id;
        let failure = |code, message: &str| {
            CallOutcome::Failed(CallFailure::Provider {
                code,
                message: message.to_owned(),
                provider_id: provider_id.clone(),
            })
        };
        let outcome = match result.outcome {
            Some(pb::call_result::Outcome::Success(success)) => {
                if success.result.len() > self.functions.max_body_bytes(&key.name) {
                    failure(
                        pb::CallErrorCode::Failed,
                        "provider result exceeds the body cap",
                    )
                } else if !is_json(&success.result) {
                    failure(
                        pb::CallErrorCode::Failed,
                        "provider result is not valid JSON",
                    )
                } else {
                    CallOutcome::Completed {
                        result: success.result,
                        media_ids: success.media_ids,
                        provider_id: provider_id.clone(),
                        queued_for: running.started_at - running.queued.enqueued_at,
                    }
                }
            }
            Some(pb::call_result::Outcome::Error(error)) => {
                let code = match error.code() {
                    pb::CallErrorCode::Unspecified => pb::CallErrorCode::Failed,
                    code => code,
                };
                failure(code, &error.message)
            }
            None => failure(pb::CallErrorCode::Failed, "empty result"),
        };
        let stats = self.stats.entry(key.clone()).or_default();
        match outcome {
            CallOutcome::Completed { .. } => stats.completed += 1,
            CallOutcome::Failed(_) => stats.failed += 1,
        }
        let _ = running.queued.reply.send(outcome);
        self.drain(&key);
    }

    /// Remove a call wherever it is, free its slot and cancel it at the provider.
    fn abandon(&mut self, id: &CallId) -> AbandonReport {
        if let Some(running) = self.running.remove(id) {
            let key = running.queued.call.key.clone();
            self.release_slot(&key, running.seq);
            if let Some(session) = self.sessions.get(&running.provider_id)
                && session.seq == running.seq
            {
                let _ = session.output.try_send(pb::ProviderServerFrame {
                    body: Some(pb::provider_server_frame::Body::Cancel(pb::CancelCall {
                        call_id: id.clone(),
                    })),
                });
            }
            self.stats.entry(key.clone()).or_default().abandoned += 1;
            self.drain(&key);
            return AbandonReport::InFlight {
                provider_id: running.provider_id,
            };
        }
        for (key, queue) in &mut self.queues {
            if let Some(position) = queue.iter().position(|queued| queued.call.id == *id) {
                queue.remove(position);
                self.stats.entry(key.clone()).or_default().abandoned += 1;
                return AbandonReport::Queued {
                    provider_connected: self.owners.contains_key(key),
                };
            }
        }
        AbandonReport::Unknown
    }

    fn list(&self) -> Vec<pb::FunctionStatus> {
        let keys: BTreeSet<&FunctionKey> = self
            .owners
            .keys()
            .chain(
                self.queues
                    .iter()
                    .filter(|(_, queue)| !queue.is_empty())
                    .map(|(key, _)| key),
            )
            .chain(self.stats.keys())
            .collect();
        keys.into_iter()
            .map(|key| {
                let owner = self.owners.get(key);
                let stats = self.stats.get(key);
                pb::FunctionStatus {
                    function: Some(key.to_proto()),
                    provider_id: owner
                        .map(|owner| owner.provider_id.clone())
                        .unwrap_or_default(),
                    declaration: owner.map(|owner| owner.declaration.clone()),
                    in_flight: owner.map_or(0, |owner| owner.in_flight),
                    queued: self
                        .queues
                        .get(key)
                        .map_or(0, |queue| queue.len().try_into().unwrap_or(u32::MAX)),
                    max_concurrency: owner.map_or(0, |owner| owner.limit),
                    completed: stats.map_or(0, |stats| stats.completed),
                    failed: stats.map_or(0, |stats| stats.failed),
                    abandoned: stats.map_or(0, |stats| stats.abandoned),
                }
            })
            .collect()
    }

    /// Hand queued calls for `key` to its owner while it has free slots.
    fn drain(&mut self, key: &FunctionKey) {
        loop {
            let Some(owner) = self.owners.get(key) else {
                return;
            };
            if owner.in_flight >= owner.limit {
                return;
            }
            let Some(queued) = self.queues.get_mut(key).and_then(VecDeque::pop_front) else {
                return;
            };
            if queued.reply.is_closed() {
                self.stats.entry(key.clone()).or_default().abandoned += 1;
                continue;
            }
            let provider_id = owner.provider_id.clone();
            let seq = owner.seq;
            let session = self
                .sessions
                .get(&provider_id)
                .expect("every owner has a session");
            let frame = pb::ProviderServerFrame {
                body: Some(pb::provider_server_frame::Body::Call(pb::ProviderCall {
                    call_id: queued.call.id.clone(),
                    function: Some(key.to_proto()),
                    args: queued.call.args.clone(),
                    media_ids: queued.call.media_ids.clone(),
                    deadline_unix_ms: queued.deadline_unix_ms,
                    caller: queued.call.caller.clone(),
                })),
            };
            if session.output.try_send(frame).is_err() {
                // The provider stopped reading its stream: the session is broken.
                self.queues
                    .get_mut(key)
                    .expect("popped above")
                    .push_front(queued);
                self.drop_session(&provider_id);
                return;
            }
            self.owners.get_mut(key).expect("checked above").in_flight += 1;
            self.running.insert(
                queued.call.id.clone(),
                Running {
                    queued,
                    provider_id,
                    seq,
                    started_at: Instant::now(),
                },
            );
        }
    }

    /// End a provider's session: its functions lose their owner, idempotent
    /// in-flight calls go back to the front of their queue and the rest fail.
    fn drop_session(&mut self, provider_id: &str) {
        let Some(session) = self.sessions.remove(provider_id) else {
            return;
        };
        let mut idempotent = HashMap::new();
        for key in &session.keys {
            if let Some(owner) = self.owners.remove(key) {
                idempotent.insert(key.clone(), owner.declaration.idempotent);
            }
        }
        let lost: Vec<CallId> = self
            .running
            .iter()
            .filter(|(_, running)| running.provider_id == provider_id && running.seq == session.seq)
            .map(|(id, _)| id.clone())
            .collect();
        let (mut requeued, mut failed) = (0, 0);
        for id in lost {
            let running = self.running.remove(&id).expect("collected above");
            let key = running.queued.call.key.clone();
            if running.queued.reply.is_closed() {
                self.stats.entry(key).or_default().abandoned += 1;
            } else if idempotent.get(&key).copied().unwrap_or(false) {
                self.queues
                    .entry(key)
                    .or_default()
                    .push_front(running.queued);
                requeued += 1;
            } else {
                let _ = running
                    .queued
                    .reply
                    .send(CallOutcome::Failed(CallFailure::ProviderLost {
                        provider_id: provider_id.to_owned(),
                    }));
                self.stats.entry(key).or_default().failed += 1;
                failed += 1;
            }
        }
        warn!(provider = %provider_id, requeued, failed, "provider session ended");
    }

    /// Free the owner's slot for a call that was running under session `seq`.
    fn release_slot(&mut self, key: &FunctionKey, seq: u64) {
        if let Some(owner) = self.owners.get_mut(key)
            && owner.seq == seq
        {
            owner.in_flight = owner.in_flight.saturating_sub(1);
        }
    }
}

pub(crate) fn is_json(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde::de::IgnoredAny>(bytes).is_ok()
}

fn unix_ms(instant: SystemTime) -> i64 {
    instant
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis().try_into().unwrap_or(i64::MAX))
}
