//! The Health page of the settings panel (plan 079, on botconf's data
//! pages): the open problems, each with Ignore, what the paused directions
//! hold, the dead letters, and 🔄 Run checks.
//!
//! Routes: `""` (the summary), `problem/<key>`, `held`, `dead?p=<page>`.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use botconf_panel::{
    ActOutcome, DataAction, DataActionKind, DataInput, DataItem, DataPage, DataSource, DataViewer,
    Paging, Span, Text,
};
use wuphf_model::{Action, Dispatch, DispatchState, FlowDirection, HealthPause, Severity};
use wuphf_storage::{HeldDispatch, Storage, StoredProblem};

use crate::health::{HealthError, HealthMonitor, HealthReport};
use crate::owner_send::now_ms;

/// The panel's entry button.
pub const HEALTH_ENTRY: &str = "🩺 Health";

/// How long 🔄 Run checks waits before showing what is known so far; the
/// checks go on and the page shows their result when it is opened again.
const RUN_WAIT: Duration = Duration::from_secs(8);

/// Dead letters per page.
const DEAD_PER_PAGE: usize = 10;

const ROOT: &str = "";
const PROBLEM: &str = "problem/";
const HELD: &str = "held";
const DEAD: &str = "dead?p=";

const RUN: &str = "run";
const IGNORE: &str = "ignore";
const UNIGNORE: &str = "unignore";

type Page<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// The health pages over the running monitor.
pub struct HealthPages<S> {
    monitor: Arc<HealthMonitor<S>>,
}

impl<S> HealthPages<S> {
    pub fn new(monitor: Arc<HealthMonitor<S>>) -> Self {
        Self { monitor }
    }
}

impl<S: Storage> DataSource for HealthPages<S> {
    fn page<'a>(&'a self, _viewer: &'a DataViewer, route: &'a str) -> Page<'a, DataPage> {
        Box::pin(async move {
            let page = if route == ROOT {
                self.summary().await
            } else if let Some(key) = route.strip_prefix(PROBLEM) {
                self.problem(key).await
            } else if route == HELD {
                self.held().await
            } else if let Some(page) = route.strip_prefix(DEAD) {
                self.dead(page.parse().map_err(|_| "no such page".to_string())?)
                    .await
            } else {
                return Err("no such page".into());
            };
            page.map_err(failed)
        })
    }

    fn act<'a>(
        &'a self,
        _viewer: &'a DataViewer,
        route: &'a str,
        action: &'a str,
        _input: Option<DataInput>,
    ) -> Page<'a, ActOutcome> {
        Box::pin(async move {
            match (action, route.strip_prefix(PROBLEM)) {
                (RUN, _) => self.run().await,
                (IGNORE | UNIGNORE, Some(key)) => {
                    let report = self
                        .monitor
                        .ignore(key, action == IGNORE)
                        .await
                        .map_err(failed)?;
                    let open = report
                        .problems
                        .iter()
                        .any(|stored| stored.problem.key == key);
                    let notice = if action == IGNORE {
                        "🔕 Ignored until it passes"
                    } else {
                        "🔔 No longer ignored"
                    };
                    Ok(ActOutcome::Show {
                        route: if open {
                            route.to_owned()
                        } else {
                            ROOT.to_owned()
                        },
                        notice: Some(notice.into()),
                    })
                }
                _ => Err("this action is not offered here".into()),
            }
        })
    }
}

impl<S: Storage> HealthPages<S> {
    async fn summary(&self) -> Result<DataPage, HealthError> {
        let report = self.monitor.report().await?;
        let held = self.monitor.held().await?;
        let dead = self.monitor.dead_letters(0, 1).await?;

        let open = report
            .problems
            .iter()
            .filter(|stored| !report.ignored.contains(&stored.problem.key))
            .count();
        let pausing = report
            .problems
            .iter()
            .filter(|stored| pauses(stored, &report))
            .count();
        let mut body = Text::new();
        match (open, report.problems.len()) {
            (0, 0) => body.push_plain("✅ No open problems."),
            (0, ignored) => body.push_plain(format!("✅ No open problems ({ignored} ignored).")),
            (open, _) => body.push_plain(format!(
                "{open} open problem{}, {pausing} pausing delivery.",
                plural(open)
            )),
        };
        body.push_plain("\nChecks run when a connector connects and when the rules change.");

        // What pauses delivery first, then warnings, then what is ignored.
        let mut problems: Vec<_> = report.problems.iter().collect();
        problems.sort_by_key(|stored| (rank(stored, &report), &stored.problem.key));
        let mut items: Vec<_> = problems
            .into_iter()
            .map(|stored| DataItem {
                label: format!("{} {}", marker(stored, &report), stored.problem.text),
                route: format!("{PROBLEM}{}", stored.problem.key),
            })
            .collect();
        if !held.is_empty() {
            items.push(DataItem {
                label: format!("⏸ {} message{} held", held.len(), plural(held.len())),
                route: HELD.into(),
            });
        }
        if !dead.is_empty() {
            items.push(DataItem {
                label: "📭 Dead letters".into(),
                route: format!("{DEAD}0"),
            });
        }
        Ok(DataPage {
            title: "🩺 Health".into(),
            body,
            items,
            actions: vec![DataAction {
                id: RUN.into(),
                label: "🔄 Run checks".into(),
                kind: DataActionKind::Run,
            }],
            paging: None,
            back: None,
        })
    }

    async fn problem(&self, key: &str) -> Result<DataPage, HealthError> {
        let report = self.monitor.report().await?;
        let Some(stored) = report
            .problems
            .iter()
            .find(|stored| stored.problem.key == key)
        else {
            return Ok(DataPage {
                title: "🩺 Health".into(),
                body: Text::plain("✅ This problem has cleared."),
                back: Some(ROOT.into()),
                ..DataPage::default()
            });
        };
        let ignored = report.ignored.contains(key);
        let mut body = Text::new();
        body.push_plain(format!("Seen since {}.\n", ago(stored.since_ms)));
        match (&stored.problem.severity, ignored) {
            (_, true) => body.push_plain(
                "🔕 Ignored until it passes once: nothing is paused for it, and no notice is sent.",
            ),
            (Severity::Pause(pause), false) => body.push_plain(format!(
                "⏸ Pauses {}: messages are held, and delivered when it passes (or dropped after 7 \
                 days).",
                direction(pause)
            )),
            (Severity::Warn, false) => body.push_plain("⚠️ A warning: nothing is paused."),
        };
        body.push_plain("\n\nKey: ").push(Span::code(key));
        let action = if ignored {
            DataAction {
                id: UNIGNORE.into(),
                label: "🔔 Stop ignoring".into(),
                kind: DataActionKind::Run,
            }
        } else {
            DataAction {
                id: IGNORE.into(),
                label: "🔕 Ignore until it passes".into(),
                kind: DataActionKind::Run,
            }
        };
        Ok(DataPage {
            title: stored.problem.text.clone(),
            body,
            actions: vec![action],
            back: Some(ROOT.into()),
            ..DataPage::default()
        })
    }

    async fn held(&self) -> Result<DataPage, HealthError> {
        let held = self.monitor.held().await?;
        let mut by_pause: BTreeMap<&HealthPause, (usize, i64)> = BTreeMap::new();
        for HeldDispatch {
            pause, held_at_ms, ..
        } in &held
        {
            let (count, oldest) = by_pause.entry(pause).or_insert((0, *held_at_ms));
            *count += 1;
            *oldest = (*oldest).min(*held_at_ms);
        }
        let mut body = Text::new();
        if by_pause.is_empty() {
            body.push_plain("Nothing is held.");
        }
        for (pause, (count, oldest)) in &by_pause {
            body.push(Span::bold(format!("{count} held")))
                .push_plain(format!(
                    " for {}, the oldest {}.\n",
                    direction(pause),
                    ago(*oldest)
                ));
        }
        if !by_pause.is_empty() {
            body.push_plain("\nThey are delivered in order when the check passes.");
        }
        Ok(DataPage {
            title: "⏸ Held messages".into(),
            body,
            back: Some(ROOT.into()),
            ..DataPage::default()
        })
    }

    async fn dead(&self, page: usize) -> Result<DataPage, HealthError> {
        let mut letters = self
            .monitor
            .dead_letters(page * DEAD_PER_PAGE, DEAD_PER_PAGE + 1)
            .await?;
        let more = letters.len() > DEAD_PER_PAGE;
        letters.truncate(DEAD_PER_PAGE);
        let mut body = Text::new();
        if letters.is_empty() {
            body.push_plain("No dead letters.");
        }
        for letter in &letters {
            body.push_plain(format!("• {}\n", dead_letter(letter)));
        }
        Ok(DataPage {
            title: "📭 Dead letters".into(),
            body: if letters.is_empty() {
                body
            } else {
                let mut text =
                    Text::plain("Dispatches that can never be delivered, newest first:\n\n");
                text.append(body);
                text
            },
            paging: Some(Paging {
                prev: page.checked_sub(1).map(|prev| format!("{DEAD}{prev}")),
                next: more.then(|| format!("{DEAD}{}", page + 1)),
            }),
            back: Some(ROOT.into()),
            ..DataPage::default()
        })
    }

    /// Runs the checks, waiting a little: a slow probe goes on, and its
    /// result shows the next time the page is opened.
    async fn run(&self) -> Result<ActOutcome, String>
    where
        S: Storage,
    {
        let monitor = Arc::clone(&self.monitor);
        let checks = tokio::spawn(async move { monitor.run().await });
        let notice = match tokio::time::timeout(RUN_WAIT, checks).await {
            Ok(Ok(Ok(report))) => checked(&report),
            Ok(Ok(Err(error))) => return Err(failed(error)),
            Ok(Err(error)) => return Err(format!("the checks stopped: {error}")),
            Err(_) => "🔄 Still checking; open the page again in a moment".into(),
        };
        Ok(ActOutcome::Show {
            route: ROOT.into(),
            notice: Some(notice),
        })
    }
}

/// Whether `stored` pauses delivery now.
fn pauses(stored: &StoredProblem, report: &HealthReport) -> bool {
    matches!(stored.problem.severity, Severity::Pause(_))
        && !report.ignored.contains(&stored.problem.key)
}

/// 0 for what pauses, 1 for a warning, 2 for what is ignored.
fn rank(stored: &StoredProblem, report: &HealthReport) -> u8 {
    if report.ignored.contains(&stored.problem.key) {
        2
    } else if pauses(stored, report) {
        0
    } else {
        1
    }
}

fn marker(stored: &StoredProblem, report: &HealthReport) -> &'static str {
    ["⏸", "⚠️", "🔕"][usize::from(rank(stored, report))]
}

fn checked(report: &HealthReport) -> String {
    let open = report
        .problems
        .iter()
        .filter(|stored| !report.ignored.contains(&stored.problem.key))
        .count();
    match open {
        0 => "✅ Checked: no open problems".into(),
        open => format!("🔄 Checked: {open} open problem{}", plural(open)),
    }
}

/// A paused direction, for the owner: which rule, which way, into which chat.
fn direction(pause: &HealthPause) -> String {
    let way = match pause.direction {
        FlowDirection::Forward => "to the mirror",
        FlowDirection::Back => "back to the source",
    };
    format!(
        "rule {} {way} ({} {})",
        pause.rule.0, pause.connector.0, pause.chat_id
    )
}

/// One dead letter: what, where, and why.
fn dead_letter(letter: &Dispatch) -> String {
    let what = match &letter.action {
        Action::Send { .. } => "message",
        Action::Edit { .. } => "edit",
        Action::Delete { .. } => "delete",
        Action::React { .. } => "reaction",
        Action::Receipt { .. } => "receipt",
        Action::Refuse { .. } => "refusal note",
        Action::Post { .. } => "post",
        Action::OpenPanel { .. } => "panel",
        Action::RenameThread { .. } => "topic rename",
        Action::Note { .. } => "note",
        Action::SetChatState { .. } => "chat state",
    };
    let reason = match &letter.state {
        DispatchState::Failed { reason } => reason.as_str(),
        _ => "failed",
    };
    format!(
        "{what} to {} {}: {reason}",
        letter.to.connector_id.0, letter.to.address.chat_id
    )
}

/// How long ago `then_ms` was, roughly.
fn ago(then_ms: i64) -> String {
    let minutes = (now_ms() - then_ms).max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn failed(error: impl std::fmt::Display) -> String {
    format!("the health checks could not be read: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::tests::fixture;
    use wuphf_model::{
        ChatAddress, ConnectorId, ConversationId, EndpointId, Problem, RuleId, Subject, Target,
    };
    use wuphf_storage::{DispatchStore, HealthStore, InMemoryStorage};

    const KEY: &str = "target-not-forum:mirror:telegram-main:telegram:channel:100";

    fn pause() -> HealthPause {
        HealthPause {
            rule: RuleId("mirror".into()),
            direction: FlowDirection::Forward,
            connector: ConnectorId("telegram-main".into()),
            chat_id: "telegram:channel:100".into(),
        }
    }

    /// A problem about a chat no probe has answered, so a check keeps it.
    fn stored(key: &str, severity: Severity, text: &str) -> StoredProblem {
        StoredProblem {
            problem: Problem {
                key: key.into(),
                severity,
                subject: Subject::Chat {
                    connector: ConnectorId("telegram-main".into()),
                    chat_id: "telegram:channel:100".into(),
                },
                text: text.into(),
            },
            since_ms: now_ms() - 2 * 60 * 60 * 1000,
        }
    }

    fn note() -> Dispatch {
        Dispatch::new(
            Target {
                endpoint_id: EndpointId::new(),
                connector_id: ConnectorId("telegram-main".into()),
                address: ChatAddress::new("telegram:channel:100", None),
                create_thread: None,
            },
            ConversationId::new(),
            Action::Note {
                text: "hi".into(),
                attachments: Vec::new(),
            },
            None,
        )
    }

    async fn pages() -> (HealthPages<InMemoryStorage>, Arc<InMemoryStorage>) {
        let fixture = fixture().await;
        (HealthPages::new(fixture.monitor), fixture.storage)
    }

    async fn page(pages: &HealthPages<InMemoryStorage>, route: &str) -> DataPage {
        pages.page(&DataViewer::default(), route).await.unwrap()
    }

    #[tokio::test]
    async fn a_healthy_core_says_so_and_offers_the_checks() {
        let (pages, _) = pages().await;
        let summary = page(&pages, "").await;
        assert!(summary.body.to_plain().starts_with("✅ No open problems."));
        assert!(summary.items.is_empty());
        assert_eq!(summary.actions[0].id, RUN);
    }

    #[tokio::test]
    async fn the_summary_lists_problems_held_messages_and_dead_letters() {
        let (pages, storage) = pages().await;
        storage
            .set_health_problems(&[
                stored(
                    KEY,
                    Severity::Pause(pause()),
                    "The mirror group is not a forum.",
                ),
                stored("target-cannot-pin:x", Severity::Warn, "Cannot pin."),
            ])
            .await
            .unwrap();
        storage.hold_dispatch(&note(), &pause()).await.unwrap();
        storage.hold_dispatch(&note(), &pause()).await.unwrap();
        let dead = note();
        storage.enqueue_dispatch(&dead).await.unwrap();
        storage
            .fail_dispatch(dead.id, "PEER_ID_INVALID".into())
            .await
            .unwrap();

        let summary = page(&pages, "").await;
        assert_eq!(
            summary.body.to_plain().lines().next(),
            Some("2 open problems, 1 pausing delivery.")
        );
        let labels: Vec<_> = summary
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "⏸ The mirror group is not a forum.",
                "⚠️ Cannot pin.",
                "⏸ 2 messages held",
                "📭 Dead letters"
            ]
        );
        assert_eq!(summary.items[0].route, format!("problem/{KEY}"));

        let held = page(&pages, "held").await;
        assert!(
            held.body.to_plain().starts_with(
                "2 held for rule mirror to the mirror (telegram-main telegram:channel:100)"
            ),
            "{}",
            held.body.to_plain()
        );
        let letters = page(&pages, "dead?p=0").await;
        assert!(
            letters
                .body
                .to_plain()
                .contains("• note to telegram-main telegram:channel:100: PEER_ID_INVALID")
        );
        assert_eq!(letters.paging.unwrap().next, None);
    }

    #[tokio::test]
    async fn a_problem_is_ignored_and_unignored_from_its_page() {
        let (pages, storage) = pages().await;
        storage
            .set_health_problems(&[stored(KEY, Severity::Pause(pause()), "Not a forum.")])
            .await
            .unwrap();
        let route = format!("problem/{KEY}");
        let problem = page(&pages, &route).await;
        assert!(problem.body.to_plain().contains("Seen since 2 h ago."));
        assert!(
            problem
                .body
                .to_plain()
                .contains("⏸ Pauses rule mirror to the mirror")
        );
        assert_eq!(problem.actions[0].id, IGNORE);

        let viewer = DataViewer::default();
        let ignored = pages.act(&viewer, &route, IGNORE, None).await.unwrap();
        assert_eq!(
            ignored,
            ActOutcome::Show {
                route: route.clone(),
                notice: Some("🔕 Ignored until it passes".into()),
            }
        );
        assert!(storage.health_ignores().await.unwrap().contains(KEY));
        let problem = page(&pages, &route).await;
        assert!(
            problem
                .body
                .to_plain()
                .starts_with("Seen since 2 h ago.\n🔕 Ignored")
        );
        assert_eq!(problem.actions[0].id, UNIGNORE);
        assert!(page(&pages, "").await.items[0].label.starts_with("🔕"));

        pages.act(&viewer, &route, UNIGNORE, None).await.unwrap();
        assert!(storage.health_ignores().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cleared_problem_s_page_says_so() {
        let (pages, _) = pages().await;
        let gone = page(&pages, "problem/nothing").await;
        assert_eq!(gone.body.to_plain(), "✅ This problem has cleared.");
        assert!(gone.actions.is_empty());
    }

    #[tokio::test]
    async fn dead_letters_are_paged_newest_first() {
        let (pages, storage) = pages().await;
        for n in 0..(DEAD_PER_PAGE + 2) {
            let dead = note();
            storage.enqueue_dispatch(&dead).await.unwrap();
            storage
                .fail_dispatch(dead.id, format!("reason {n}"))
                .await
                .unwrap();
        }
        let first = page(&pages, "dead?p=0").await;
        assert_eq!(first.body.to_plain().matches("• ").count(), DEAD_PER_PAGE);
        let paging = first.paging.unwrap();
        assert_eq!(
            (paging.prev, paging.next.as_deref()),
            (None, Some("dead?p=1"))
        );
        let second = page(&pages, "dead?p=1").await;
        assert_eq!(second.body.to_plain().matches("• ").count(), 2);
        assert_eq!(second.paging.unwrap().prev.as_deref(), Some("dead?p=0"));
    }

    #[tokio::test]
    async fn running_the_checks_reports_what_they_found() {
        let (pages, _) = pages().await;
        let outcome = pages
            .act(&DataViewer::default(), "", RUN, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ActOutcome::Show {
                route: String::new(),
                notice: Some("✅ Checked: no open problems".into()),
            }
        );
    }

    #[tokio::test]
    async fn an_unknown_page_is_refused() {
        let (pages, _) = pages().await;
        assert!(
            pages
                .page(&DataViewer::default(), "elsewhere")
                .await
                .is_err()
        );
        assert!(
            pages
                .page(&DataViewer::default(), "dead?p=x")
                .await
                .is_err()
        );
    }
}
