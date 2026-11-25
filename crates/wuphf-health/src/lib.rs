//! The core's health checks (plan 079), as pure functions: which chats to
//! probe for the rules, and the [`Problem`]s found in what was observed.
//!
//! Nothing here does I/O or reads a clock. The runtime gathers the facts
//! (probes, tools, liveness), calls [`evaluate`], and stores, notifies and
//! pauses by the result.
//!
//! What a rule needs, per direction:
//!
//! - **Forward** (source → destination) needs the destination chat to exist,
//!   hold the account and let it post. A destination that provisions threads
//!   (a many-to-one into a connector with threads, or a one-to-one into a
//!   named thread) must be a forum, and one that creates topics must let the
//!   account create them.
//! - **Back** (replies typed at the destination) needs each source chat the
//!   rule names to exist, hold the account and let it post. A many-to-one
//!   over every chat names none.
//!
//! A fact that is unknown, or a chat that was not probed, is never a
//! problem.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use wuphf_model::{
    ChatAddress, ChatFacts, ChatFilter, ChatRef, Connector, ConnectorId, Fact, FlowDirection,
    HealthPause, Problem, Rule, RuleId, RuleKind, Severity, Subject, ToolStatus,
};

/// Facts or titles per connector and chat id.
pub type ByChat<T> = BTreeMap<(ConnectorId, String), T>;

/// How long a connector may be away before it is a problem.
pub const OFFLINE_AFTER_MS: i64 = 15 * 60 * 1000;

/// Everything the checks read.
pub struct Observed<'a> {
    pub rules: &'a [Rule],
    pub connectors: &'a [Connector],
    /// Whether pins cross between chats anywhere (`chat_state`), which makes
    /// a missing pin right worth a warning.
    pub pins_synced: bool,
    /// The latest facts per connector and chat id.
    pub facts: &'a ByChat<ChatFacts>,
    /// Chat titles, for the texts; a chat without one is named by its id.
    pub titles: &'a ByChat<String>,
    /// The tools each process reported, by process (a connector or provider
    /// id).
    pub tools: &'a BTreeMap<String, Vec<ToolStatus>>,
    /// Connectors away for longer than [`OFFLINE_AFTER_MS`], with the minutes.
    pub offline: &'a [(ConnectorId, i64)],
    /// Configured functions that no connected provider offers.
    pub unavailable_functions: &'a [String],
    /// Chat references in the settings that no connector knows, with the
    /// setting naming each.
    pub unknown_chats: &'a [(String, ChatRef)],
}

impl Default for Observed<'_> {
    /// Nothing observed: no rules, facts, tools or wiring problems.
    fn default() -> Self {
        const NO_FACTS: &ByChat<ChatFacts> = &BTreeMap::new();
        const NO_TITLES: &ByChat<String> = &BTreeMap::new();
        const NO_TOOLS: &BTreeMap<String, Vec<ToolStatus>> = &BTreeMap::new();
        Self {
            rules: &[],
            connectors: &[],
            pins_synced: false,
            facts: NO_FACTS,
            titles: NO_TITLES,
            tools: NO_TOOLS,
            offline: &[],
            unavailable_functions: &[],
            unknown_chats: &[],
        }
    }
}

/// The chats to probe on each connector for `rules`: every destination chat,
/// and every source chat a rule names.
pub fn probe_targets(rules: &[Rule]) -> BTreeMap<ConnectorId, BTreeSet<String>> {
    let mut targets: BTreeMap<ConnectorId, BTreeSet<String>> = BTreeMap::new();
    for rule in rules {
        for (connector, chat, _) in rule_chats(rule) {
            targets
                .entry(connector.clone())
                .or_default()
                .insert(chat.chat_id.clone());
        }
    }
    targets
}

/// Which end of a rule a chat is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    /// The destination; `threads` says whether the rule puts its messages
    /// in threads there (then, on a connector with threads, it must be a
    /// forum, and one that creates topics must allow creating them).
    Destination {
        threads: bool,
    },
    Source,
}

/// Every chat of `rule` that is checked, with its end. Threads are decided
/// by the caller from the connector's capabilities.
fn rule_chats(rule: &Rule) -> Vec<(&ConnectorId, &ChatAddress, End)> {
    match &rule.flow {
        RuleKind::OneToOne {
            source_connector,
            source_chat,
            destination_connector,
            destination_chat,
        } => vec![
            (
                destination_connector,
                destination_chat,
                End::Destination {
                    threads: destination_chat.thread_id.is_some(),
                },
            ),
            (source_connector, source_chat, End::Source),
        ],
        RuleKind::ManyToOne {
            source_connector,
            source_chats_filter,
            destination_connector,
            destination_chat,
        } => {
            let mut chats = vec![(
                destination_connector,
                destination_chat,
                End::Destination { threads: true },
            )];
            if let ChatFilter::Only { chats: sources } = source_chats_filter {
                chats.extend(
                    sources
                        .iter()
                        .map(|chat| (source_connector, chat, End::Source)),
                );
            }
            chats
        }
    }
}

/// The problems in what was observed, sorted by key.
pub fn evaluate(observed: &Observed<'_>) -> Vec<Problem> {
    let mut problems = BTreeMap::new();
    let capabilities: HashMap<&ConnectorId, &Connector> = observed
        .connectors
        .iter()
        .map(|connector| (&connector.id, connector))
        .collect();
    for rule in observed.rules {
        for (connector, chat, end) in rule_chats(rule) {
            let Some(facts) = observed
                .facts
                .get(&(connector.clone(), chat.chat_id.clone()))
            else {
                continue;
            };
            let caps = capabilities
                .get(connector)
                .map(|connector| connector.capabilities)
                .unwrap_or_default();
            let name = chat_name(observed, connector, &chat.chat_id);
            for check in chat_checks(end, &caps, observed.pins_synced) {
                if !(check.fact)(facts).is_no() {
                    continue;
                }
                let mut text = format!("Rule {}: {}", rule.id, (check.text)(connector, &name));
                if !facts.detail.is_empty() {
                    text.push_str(&format!(" ({})", facts.detail));
                }
                let severity = if check.pauses {
                    let direction = match end {
                        End::Destination { .. } => FlowDirection::Forward,
                        End::Source => FlowDirection::Back,
                    };
                    Severity::Pause(HealthPause {
                        rule: rule.id.clone(),
                        direction,
                        connector: connector.clone(),
                        chat_id: chat.chat_id.clone(),
                    })
                } else {
                    Severity::Warn
                };
                text.push('.');
                let key = chat_key(check.key, &rule.id, connector, &chat.chat_id);
                let subject = Subject::Chat {
                    connector: connector.clone(),
                    chat_id: chat.chat_id.clone(),
                };
                problems.insert(
                    key.clone(),
                    Problem {
                        key,
                        severity,
                        subject,
                        text,
                    },
                );
            }
        }
    }
    for (process, tools) in observed.tools {
        for tool in tools.iter().filter(|tool| !tool.available) {
            let key = format!("tool-missing:{process}:{}", tool.name);
            let mut text = format!("{process}: {} is not installed", tool.name);
            if !tool.needed_for.is_empty() {
                text.push_str(&format!(", so {} do not work", tool.needed_for));
            }
            text.push('.');
            warn(&mut problems, key, Subject::Process(process.clone()), text);
        }
    }
    for (connector, minutes) in observed.offline {
        warn(
            &mut problems,
            format!("connector-offline:{connector}"),
            Subject::Wiring,
            format!(
                "{connector} has not been connected for {minutes} minutes; messages for it wait."
            ),
        );
    }
    for function in observed.unavailable_functions {
        warn(
            &mut problems,
            format!("function-unavailable:{function}"),
            Subject::Wiring,
            format!("No connected provider offers {function}; calls to it fail at once."),
        );
    }
    for (setting, chat) in observed.unknown_chats {
        warn(
            &mut problems,
            format!(
                "unknown-chat:{setting}:{}:{}",
                chat.connector_id, chat.address.chat_id
            ),
            Subject::Wiring,
            format!(
                "{setting} names {}:{}, a chat {} has never reported.",
                chat.connector_id, chat.address.chat_id, chat.connector_id
            ),
        );
    }
    problems.into_values().collect()
}

fn warn(problems: &mut BTreeMap<String, Problem>, key: String, subject: Subject, text: String) {
    problems.insert(
        key.clone(),
        Problem {
            key,
            severity: Severity::Warn,
            subject,
            text,
        },
    );
}

fn chat_key(check: &str, rule: &RuleId, connector: &ConnectorId, chat: &str) -> String {
    format!("{check}:{rule}:{connector}:{chat}")
}

fn chat_name(observed: &Observed<'_>, connector: &ConnectorId, chat: &str) -> String {
    match observed.titles.get(&(connector.clone(), chat.to_owned())) {
        Some(title) => format!("\"{title}\" ({chat})"),
        None => chat.to_owned(),
    }
}

/// One check of a chat: the fact it reads, whether a `No` pauses, and what
/// the owner reads.
struct ChatCheck {
    key: &'static str,
    fact: fn(&ChatFacts) -> Fact,
    pauses: bool,
    text: fn(&ConnectorId, &str) -> String,
}

fn chat_checks(
    end: End,
    caps: &wuphf_model::ConnectorCapabilities,
    pins_synced: bool,
) -> Vec<ChatCheck> {
    let (prefix, threads) = match end {
        End::Destination { threads } => ("target", threads && caps.supports_threads),
        End::Source => ("source", false),
    };
    let mut checks = vec![
        ChatCheck {
            key: if prefix == "target" {
                "target-missing"
            } else {
                "source-missing"
            },
            fact: |facts| facts.exists,
            pauses: true,
            text: |connector, chat| format!("{connector} cannot find chat {chat}"),
        },
        ChatCheck {
            key: if prefix == "target" {
                "target-not-member"
            } else {
                "source-not-member"
            },
            fact: |facts| facts.member,
            pauses: true,
            text: |connector, chat| format!("{connector}'s account is not in {chat}"),
        },
        ChatCheck {
            key: if prefix == "target" {
                "target-cannot-send"
            } else {
                "source-cannot-send"
            },
            fact: |facts| facts.can_send,
            pauses: true,
            text: |connector, chat| format!("{connector}'s account may not post in {chat}"),
        },
    ];
    if threads {
        checks.push(ChatCheck {
            key: "target-not-forum",
            fact: |facts| facts.forum,
            pauses: true,
            text: |connector, chat| {
                format!(
                    "{chat} on {connector} has no topics (turn on Topics in the group's settings)"
                )
            },
        });
        if caps.supports_topic_create {
            checks.push(ChatCheck {
                key: "target-cannot-create-topics",
                fact: |facts| facts.can_create_topics,
                pauses: true,
                text: |connector, chat| {
                    format!(
                        "{connector}'s account may not create topics in {chat} (give it the Manage \
                         Topics admin right)"
                    )
                },
            });
        }
    }
    if pins_synced && matches!(end, End::Destination { .. }) {
        checks.push(ChatCheck {
            key: "target-cannot-pin",
            fact: |facts| facts.can_pin,
            pauses: false,
            text: |connector, chat| {
                format!("{connector}'s account may not pin in {chat}, so pins do not sync")
            },
        });
    }
    checks
}

/// The pauses in effect: those of the problems not ignored.
pub fn effective_pauses<'a>(
    problems: &'a [Problem],
    ignored: &BTreeSet<String>,
) -> BTreeSet<&'a HealthPause> {
    problems
        .iter()
        .filter(|problem| !ignored.contains(&problem.key))
        .filter_map(Problem::pause)
        .collect()
}

#[cfg(test)]
mod tests;
