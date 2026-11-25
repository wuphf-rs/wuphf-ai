//! The health monitor (plan 079): gathers what the checks read, runs them
//! (`wuphf-health`), and acts on the result.
//!
//! - **Facts** come from probes of the live connector sessions: when a
//!   connector connects (its own chats), when the rules change (every chat),
//!   and on demand ([`HealthMonitor::run`]). A fact a connector does not
//!   know keeps the one it reported before, and a problem about a chat no
//!   probe has answered since startup stands: an unknown never pauses or
//!   resumes anything.
//! - **Problems** are stored with when they were first seen. A new one is
//!   told to the owner once; a cleared one gets a ✅, with what its pause
//!   held.
//! - **Pauses** are the problems' that the owner does not ignore. The core
//!   holds dispatches into them; a pause that ends releases what it held,
//!   dropping what was held for longer than [`HOLD_FOR_MS`]. An ignore
//!   lapses once its problem clears.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};
use wuphf_health::{ByChat, OFFLINE_AFTER_MS, Observed, effective_pauses, evaluate, probe_targets};
use wuphf_model::{
    ChatFacts, ConnectorId, Fact, FlowDirection, HealthPause, Problem, Subject, ToolStatus,
};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::health_transport_server::HealthTransport;
use wuphf_settings::SettingsStore;
use wuphf_storage::{HeldDispatch, Storage, StorageError, StoredProblem};

use crate::live_sessions::{LiveSessions, PROBE_TIMEOUT, SessionEvent};
use crate::owner_send::{OwnerMessageSender, OwnerSendError, now_ms};

/// How long a paused direction holds a message before dropping it.
pub const HOLD_FOR_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// How often the checks run again without probing, so a connector that
/// stays away is reported once it has been away for long enough.
const RECHECK_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum HealthError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Send(#[from] OwnerSendError),
}

/// The open problems and the keys of the ignored ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HealthReport {
    pub problems: Vec<StoredProblem>,
    pub ignored: BTreeSet<String>,
}

/// Where the checks' facts and wiring come from besides the connectors:
/// the providers' tools and the functions no connected provider offers.
pub trait Wiring: Send + Sync + 'static {
    /// The tools each connected provider reported, by provider id.
    fn provider_tools(&self) -> BTreeMap<String, Vec<ToolStatus>>;
    /// Configured functions that no connected provider offers now.
    fn unavailable_functions(&self) -> Vec<String>;
}

/// Runs the health checks and acts on them; see the module docs.
pub struct HealthMonitor<S> {
    storage: Arc<S>,
    sessions: Arc<LiveSessions>,
    sender: OwnerMessageSender<S>,
    settings: Arc<SettingsStore>,
    wiring: Option<Arc<dyn Wiring>>,
    started_ms: i64,
    state: Mutex<Observations>,
    /// One check at a time, so two triggers cannot both announce a problem.
    checking: tokio::sync::Mutex<()>,
}

/// What the probes and sessions told the monitor since it started.
#[derive(Default)]
struct Observations {
    facts: ByChat<ChatFacts>,
    /// The chats some probe has answered about since startup.
    probed: BTreeSet<(ConnectorId, String)>,
}

impl<S> HealthMonitor<S>
where
    S: Storage,
{
    pub fn new(
        storage: Arc<S>,
        sessions: Arc<LiveSessions>,
        sender: OwnerMessageSender<S>,
        settings: Arc<SettingsStore>,
        wiring: Option<Arc<dyn Wiring>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            storage,
            sessions,
            sender,
            settings,
            wiring,
            started_ms: now_ms(),
            state: Mutex::default(),
            checking: tokio::sync::Mutex::new(()),
        })
    }

    /// Hold what the stored problems pause, before anything is routed.
    pub async fn restore(&self) -> Result<(), HealthError> {
        let stored = self.storage.health_problems().await?;
        let ignored = self.storage.health_ignores().await?;
        self.sender.set_health_pauses(pauses_of(&stored, &ignored));
        Ok(())
    }

    /// Follow sessions, rule changes and the clock, checking on each, until
    /// the runtime stops.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let monitor = Arc::clone(self);
        tokio::spawn(async move { monitor.follow().await })
    }

    async fn follow(self: Arc<Self>) {
        let mut sessions = self.sessions.subscribe();
        let mut settings = self.settings.subscribe();
        let mut targets = probe_targets(self.settings.current().derived.routing.rules());
        let mut recheck = tokio::time::interval(RECHECK_EVERY);
        loop {
            let outcome = tokio::select! {
                event = sessions.recv() => match event {
                    Ok(SessionEvent::Attached(connector)) => {
                        self.probe(Some(&ConnectorId(connector))).await;
                        self.check().await
                    }
                    Ok(SessionEvent::Detached(_)) => self.check().await,
                    Err(RecvError::Lagged(_)) => self.run().await.map(drop),
                    Err(RecvError::Closed) => return,
                },
                changed = settings.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let now = probe_targets(settings.borrow_and_update().derived.routing.rules());
                    if now == targets {
                        continue;
                    }
                    targets = now;
                    self.run().await.map(drop)
                }
                _ = recheck.tick() => self.check().await,
            };
            if let Err(error) = outcome {
                warn!(%error, "health check failed");
            }
        }
    }

    /// Probe every connected connector's rule chats, then check.
    pub async fn run(&self) -> Result<HealthReport, HealthError> {
        self.probe(None).await;
        self.check().await?;
        self.report().await
    }

    /// The open problems, as last checked.
    pub async fn report(&self) -> Result<HealthReport, HealthError> {
        Ok(HealthReport {
            problems: self.storage.health_problems().await?,
            ignored: self.storage.health_ignores().await?,
        })
    }

    /// The dispatches held for paused directions, in the order they were
    /// held.
    pub async fn held(&self) -> Result<Vec<HeldDispatch>, HealthError> {
        Ok(self.storage.held_dispatches().await?)
    }

    /// Failed dispatches, newest first: `limit` of them after `offset`.
    pub async fn dead_letters(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<wuphf_model::Dispatch>, HealthError> {
        Ok(self.storage.dead_letters(offset, limit).await?)
    }

    /// Ignore the problem `key` until it passes once, or stop ignoring it,
    /// and act on the change at once.
    pub async fn ignore(&self, key: &str, ignored: bool) -> Result<HealthReport, HealthError> {
        self.storage.set_health_ignored(key, ignored).await?;
        self.check().await?;
        self.report().await
    }

    /// Ask the live sessions (all, or only `only`) about their rule chats and
    /// keep what they answer.
    async fn probe(&self, only: Option<&ConnectorId>) {
        let routing = Arc::clone(&self.settings.current().derived.routing);
        let connected: BTreeSet<_> = self.sessions.connected().into_iter().collect();
        let mut probes = JoinSet::new();
        for (connector, chats) in probe_targets(routing.rules()) {
            if !connected.contains(&connector.0) || only.is_some_and(|only| *only != connector) {
                continue;
            }
            let sessions = Arc::clone(&self.sessions);
            probes.spawn(async move {
                let answer = sessions
                    .probe(&connector.0, chats.into_iter().collect(), PROBE_TIMEOUT)
                    .await;
                (connector, answer)
            });
        }
        while let Some(joined) = probes.join_next().await {
            let Ok((connector, answer)) = joined else {
                continue;
            };
            match answer {
                Ok(result) => {
                    let mut state = self.state.lock().unwrap();
                    for chat in result.chats {
                        let key = (connector.clone(), chat.chat_id.clone());
                        let known = state.facts.remove(&key).unwrap_or_default();
                        state
                            .facts
                            .insert(key.clone(), merge(known, facts_from_wire(chat)));
                        state.probed.insert(key);
                    }
                }
                Err(error) => debug!(connector = %connector, %error, "probe unanswered"),
            }
        }
    }

    /// Run the checks on what is known and act on what changed.
    async fn check(&self) -> Result<(), HealthError> {
        let _one_at_a_time = self.checking.lock().await;
        let snapshot = self.settings.current();
        let routing = Arc::clone(&snapshot.derived.routing);
        let connectors = snapshot.derived.connectors.clone();
        let now = now_ms();

        let mut tools = self.sessions.tools();
        let mut unavailable = Vec::new();
        if let Some(wiring) = &self.wiring {
            tools.extend(wiring.provider_tools());
            unavailable = wiring.unavailable_functions();
        }
        // One read of the registry: a session ending between two reads must
        // not look away since startup.
        let presence = self.sessions.presence();
        let offline: Vec<_> = connectors
            .iter()
            .filter_map(|connector| {
                let away = now - presence.away_since(&connector.id.0, self.started_ms)?;
                (away >= OFFLINE_AFTER_MS).then(|| (connector.id.clone(), away / 60_000))
            })
            .collect();
        let (facts, probed) = {
            let state = self.state.lock().unwrap();
            (state.facts.clone(), state.probed.clone())
        };
        let mut titles = ByChat::new();
        for (connector, chat_id) in facts.keys() {
            if let Some(title) = self
                .storage
                .chat_profile(connector, chat_id)
                .await?
                .and_then(|profile| profile.title)
            {
                titles.insert((connector.clone(), chat_id.clone()), title);
            }
        }
        let found = evaluate(&Observed {
            rules: routing.rules(),
            connectors: &connectors,
            pins_synced: routing.pins_synced(),
            facts: &facts,
            titles: &titles,
            tools: &tools,
            offline: &offline,
            unavailable_functions: &unavailable,
            unknown_chats: &[],
        });

        let stored = self.storage.health_problems().await?;
        let ignored_before = self.storage.health_ignores().await?;
        let mut open: BTreeMap<String, Problem> = found
            .into_iter()
            .map(|problem| (problem.key.clone(), problem))
            .collect();
        // What could not be re-checked stands.
        let unknown = |subject: &Subject| match subject {
            Subject::Chat { connector, chat_id } => {
                !probed.contains(&(connector.clone(), chat_id.clone()))
            }
            Subject::Process(process) => !tools.contains_key(process),
            Subject::Wiring => false,
        };
        for kept in &stored {
            if !open.contains_key(&kept.problem.key) && unknown(&kept.problem.subject) {
                open.insert(kept.problem.key.clone(), kept.problem.clone());
            }
        }
        let since: HashMap<_, _> = stored
            .iter()
            .map(|stored| (stored.problem.key.clone(), stored.since_ms))
            .collect();
        let now_open: Vec<StoredProblem> = open
            .values()
            .map(|problem| StoredProblem {
                since_ms: since.get(&problem.key).copied().unwrap_or(now),
                problem: problem.clone(),
            })
            .collect();
        let cleared: Vec<&StoredProblem> = stored
            .iter()
            .filter(|stored| !open.contains_key(&stored.problem.key))
            .collect();
        let mut ignored = ignored_before.clone();
        for gone in &cleared {
            if ignored.remove(&gone.problem.key) {
                self.storage
                    .set_health_ignored(&gone.problem.key, false)
                    .await?;
            }
        }
        self.storage.set_health_problems(&now_open).await?;

        let paused_before = pauses_of(&stored, &ignored_before);
        let paused_now = pauses_of(&now_open, &ignored);
        self.sender.set_health_pauses(paused_now.clone());
        let mut released = BTreeMap::new();
        for pause in paused_before.difference(&paused_now) {
            let outcome = self.sender.release_held(pause, now - HOLD_FOR_MS).await?;
            info!(
                rule = %pause.rule,
                direction = pause.direction.as_str(),
                sent = outcome.dispatches.len(),
                dropped = outcome.dropped,
                "health pause ended",
            );
            released.insert(pause.clone(), outcome);
        }

        for problem in open.values() {
            if since.contains_key(&problem.key) || ignored.contains(&problem.key) {
                continue;
            }
            let mut text = format!("⚠️ {}", problem.text);
            if let Some(pause) = problem.pause() {
                text.push_str(match pause.direction {
                    FlowDirection::Forward => " Messages into it are held until it is fixed.",
                    FlowDirection::Back => " Replies to it are held until it is fixed.",
                });
            }
            self.sender.announce(&text).await?;
        }
        for gone in cleared {
            if ignored_before.contains(&gone.problem.key) {
                continue;
            }
            let mut text = format!("✅ Fixed: {}", gone.problem.text);
            if let Some(outcome) = gone
                .problem
                .pause()
                .and_then(|pause| released.remove(pause))
            {
                text.push_str(&format!(" {} held messages sent", outcome.dispatches.len()));
                if outcome.dropped > 0 {
                    text.push_str(&format!(", {} older than 7 days dropped", outcome.dropped));
                }
                text.push('.');
            }
            self.sender.announce(&text).await?;
        }
        Ok(())
    }
}

/// The pauses of the problems not ignored.
fn pauses_of(problems: &[StoredProblem], ignored: &BTreeSet<String>) -> BTreeSet<HealthPause> {
    let problems: Vec<Problem> = problems
        .iter()
        .map(|stored| stored.problem.clone())
        .collect();
    effective_pauses(&problems, ignored)
        .into_iter()
        .cloned()
        .collect()
}

/// `fresh` over `known`: a fact the connector does not know now keeps the
/// one it knew.
fn merge(known: ChatFacts, fresh: ChatFacts) -> ChatFacts {
    let pick = |known: Fact, fresh: Fact| if fresh == Fact::Unknown { known } else { fresh };
    ChatFacts {
        exists: pick(known.exists, fresh.exists),
        member: pick(known.member, fresh.member),
        can_send: pick(known.can_send, fresh.can_send),
        forum: pick(known.forum, fresh.forum),
        can_create_topics: pick(known.can_create_topics, fresh.can_create_topics),
        can_pin: pick(known.can_pin, fresh.can_pin),
        can_delete_others: pick(known.can_delete_others, fresh.can_delete_others),
        detail: fresh.detail,
    }
}

fn fact_from_wire(fact: i32) -> Fact {
    match pb::Fact::try_from(fact) {
        Ok(pb::Fact::Yes) => Fact::Yes,
        Ok(pb::Fact::No) => Fact::No,
        _ => Fact::Unknown,
    }
}

fn facts_from_wire(facts: pb::ChatFacts) -> ChatFacts {
    ChatFacts {
        exists: fact_from_wire(facts.exists),
        member: fact_from_wire(facts.member),
        can_send: fact_from_wire(facts.can_send),
        forum: fact_from_wire(facts.forum),
        can_create_topics: fact_from_wire(facts.can_create_topics),
        can_pin: fact_from_wire(facts.can_pin),
        can_delete_others: fact_from_wire(facts.can_delete_others),
        detail: facts.detail,
    }
}

/// The core's `HealthTransport` service, behind the observer token.
pub struct GrpcHealthService<S> {
    monitor: Arc<HealthMonitor<S>>,
}

impl<S> GrpcHealthService<S> {
    pub fn new(monitor: Arc<HealthMonitor<S>>) -> Self {
        Self { monitor }
    }
}

#[tonic::async_trait]
impl<S> HealthTransport for GrpcHealthService<S>
where
    S: Storage,
{
    async fn run(
        &self,
        _request: Request<pb::RunHealthRequest>,
    ) -> Result<Response<pb::HealthReport>, Status> {
        let report = self
            .monitor
            .run()
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(report_to_wire(&report)))
    }
}

/// A report as `HealthTransport` returns it.
pub fn report_to_wire(report: &HealthReport) -> pb::HealthReport {
    pb::HealthReport {
        problems: report
            .problems
            .iter()
            .map(|stored| pb::HealthProblem {
                key: stored.problem.key.clone(),
                text: stored.problem.text.clone(),
                pauses: stored.problem.pause().is_some(),
                ignored: report.ignored.contains(&stored.problem.key),
                since_ms: stored.since_ms,
            })
            .collect(),
    }
}

#[cfg(test)]
pub(crate) mod tests;
