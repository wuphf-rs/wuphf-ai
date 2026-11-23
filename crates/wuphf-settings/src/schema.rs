//! The schema: how botconf validates the configuration and what it derives
//! from it.

use crate::config::Configuration;
use crate::domains;
use botconf::{Choices, Field, FixedChoice, Form, Kind, RuntimeSetting};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use wuphf_core::{ChatStateSettings, RoutingConfig};
use wuphf_functions::Functions;
use wuphf_model::{
    ChatStatePolicy, ChatStatePolicyOverride, Connector, ConnectorCapabilities, ConnectorId,
    Platform, Rule, RuleId, is_valid_connector_id,
};

/// What the core runs from, computed from a validated configuration.
pub struct Derived {
    pub routing: Arc<RoutingConfig>,
    pub connectors: Vec<Connector>,
    pub functions: Arc<Functions>,
    /// Each connector's token, read from its `token_env`. A plain map so this
    /// crate stays independent of the runtime; the server wraps it in
    /// `wuphf_runtime::auth::ConnectorTokens`.
    pub connector_tokens: HashMap<ConnectorId, String>,
    /// The owner's platform user ids on each connector (`owner_ids`): who
    /// may change settings through that connector.
    pub owners: HashMap<ConnectorId, HashSet<String>>,
}

/// Reads an environment variable by name.
type EnvReader = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Describes the core configuration to botconf.
pub struct WuphfSchema {
    /// Reads environment variables (tokens); injectable for tests.
    env: Arc<EnvReader>,
}

/// Where connectors' own runtime settings live: `connector_settings.<id>`.
/// Not `connectors.<id>`, because `[[connectors]]` is an array of file-only
/// entries, and a path into an array element is not a settings key.
pub const CONNECTOR_SETTINGS: &str = "connector_settings";

pub type SettingsStore = botconf::SettingsStore<WuphfSchema>;
pub type Snapshot = botconf::Snapshot<WuphfSchema>;

impl WuphfSchema {
    /// Reads tokens from the process environment.
    pub fn new() -> Self {
        Self::with_env(|name| std::env::var(name).ok())
    }

    /// Reads tokens through `env` instead of the process environment.
    pub fn with_env(env: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self { env: Arc::new(env) }
    }
}

impl Default for WuphfSchema {
    fn default() -> Self {
        Self::new()
    }
}

impl botconf::Schema for WuphfSchema {
    type Config = Configuration;
    type Derived = Derived;

    fn derive(&self, config: &Configuration) -> Result<Derived, String> {
        if config.connector_settings.is_some() {
            return Err(format!(
                "remove [{CONNECTOR_SETTINGS}] from the core configuration: each connector \
                 declares its own settings, based on its own config file"
            ));
        }
        let mut connectors = Vec::new();
        for connector in &config.connectors {
            if !is_valid_connector_id(&connector.id) {
                return Err(format!(
                    "connector id {:?} must not be empty or contain ':', '#', '%' or spaces",
                    connector.id
                ));
            }
            connectors.push(Connector {
                id: ConnectorId(connector.id.clone()),
                platform: connector
                    .platform
                    .parse::<Platform>()
                    .map_err(|error| error.to_string())?,
                capabilities: ConnectorCapabilities {
                    supports_threads: connector.supports_threads,
                    supports_topic_create: connector.supports_topic_create,
                    supports_disappearing_timers: connector.supports_disappearing_timers,
                },
            });
        }
        let connector_tokens = self.connector_tokens(config)?;
        let owners = owners(config)?;
        let notice_targets = config
            .notify
            .iter()
            .map(|target| target.0.clone())
            .collect();
        let routing = RoutingConfig::new(rules(config)?, connectors.clone())
            .with_notice_targets(notice_targets)
            .with_chat_state(chat_state(config)?);
        routing.validate().map_err(|error| error.to_string())?;
        let routing = Arc::new(routing);
        let functions = config
            .functions
            .clone()
            .resolve(|name| (self.env)(name))
            .map_err(|error| format!("invalid [functions] configuration: {error}"))?;
        Ok(Derived {
            routing,
            connectors,
            functions: Arc::new(functions),
            connector_tokens,
            owners,
        })
    }

    /// Connectors register their own sections below this prefix when they
    /// connect (see `wuphf_runtime::connector_settings`).
    fn dynamic_namespaces(&self) -> Vec<&'static str> {
        vec![CONNECTOR_SETTINGS]
    }

    /// The keys that may change while the core runs: the options marked
    /// `runtime` in [`Configuration`]. Everything else (the connectors,
    /// `listen`, `database_url`, the tokens, `functions`) is read once at
    /// startup, so it stays file and environment only.
    fn settings(&self) -> Vec<RuntimeSetting> {
        crate::config::description().runtime_settings()
    }
}

/// The directions a chat state can cross in, named from the source chat
/// (which need not be WhatsApp).
const DIRECTIONS: &[FixedChoice] = &[
    FixedChoice::new("off", "Off"),
    FixedChoice::new("to_mirror", "Source → mirror"),
    FixedChoice::new("to_source", "Mirror → source"),
    FixedChoice::new("both", "Both ways"),
];
pub(crate) const DIRECTION: Kind = Kind::one_of(Choices::fixed(DIRECTIONS), false, false);
const OPTIONAL_DIRECTION: Kind = Kind::one_of(Choices::fixed(DIRECTIONS), false, true);
/// An override names its chat (required, so a new one is built in a panel
/// draft) and the states it changes; an absent state takes the global value.
pub(crate) const CHAT_STATE_OVERRIDE: &Kind = &Kind::form(CHAT_STATE_FORM);
const CHAT_STATE_FORM: &Form = &Form::new(CHAT_STATE_FIELDS, "{}");
const CHAT_STATE_FIELDS: &[Field] = &[
    Field::new(
        "chat",
        "Chat (source chat or mirror topic)",
        Kind::reference(domains::CHAT, false),
    )
    .required(),
    Field::new("pin", "Pin", OPTIONAL_DIRECTION),
    Field::new("mute", "Mute", OPTIONAL_DIRECTION),
    Field::new("archive", "Archive", OPTIONAL_DIRECTION),
];

/// The `chat_state` settings the router reads: the global directions, and
/// each override keyed by its chat. Override names follow the map-entry rule.
fn chat_state(config: &Configuration) -> Result<ChatStateSettings, String> {
    let state = &config.chat_state;
    let mut overrides = Vec::new();
    for (name, entry) in &state.chats {
        if !botconf::keys::is_entry_name(name) {
            return Err(format!(
                "chat_state override {name:?} must be non-empty and must not contain '.'"
            ));
        }
        overrides.push((
            entry.chat.0.clone(),
            ChatStatePolicyOverride {
                pin: entry.pin,
                mute: entry.mute,
                archive: entry.archive,
            },
        ));
    }
    Ok(ChatStateSettings {
        global: ChatStatePolicy {
            pin: state.pin,
            mute: state.mute,
            archive: state.archive,
        },
        overrides,
    })
}

// A rule entry, edited field by field. Nested descriptors are not promoted
// to `'static`, so the kind, the form and its pieces each get a `const`.
pub(crate) const RULE: &Kind = &Kind::form(RULE_FORM);
const RULE_FORM: &Form = &Form::new(RULE_FIELDS, r#"{"kind": "many-to-one"}"#);
const RULE_KINDS: &[FixedChoice] = &[
    FixedChoice::new("one-to-one", "One chat to one chat"),
    FixedChoice::new("many-to-one", "Every chat into one, a thread each"),
];
/// Fields that do not apply to the chosen `kind` carry a `when` and stay
/// hidden (and are dropped when the kind changes). A new rule, or a rule whose
/// kind changes, needs several fields at once, so the panel builds it in a
/// draft, asks for the required fields, and stores it on Save; `derive` still
/// validates the whole entry. `only_chats` and `except_chats` share a group:
/// setting one clears the other, as a rule may not have both.
const RULE_FIELDS: &[Field] = &[
    Field::new(
        "kind",
        "Kind",
        Kind::one_of(Choices::fixed(RULE_KINDS), false, true),
    )
    .required(),
    Field::new(
        "priority",
        "Priority (lower first)",
        Kind::number(-1000, 1000, "", &[-1, 0, 1], true),
    ),
    Field::new(
        "source",
        "Source chat (one-to-one)",
        Kind::reference(domains::CHAT, false),
    )
    .when("kind", "one-to-one")
    .required(),
    Field::new(
        "source_connector",
        "Source connector (many-to-one)",
        Kind::reference(domains::CONNECTOR, false),
    )
    .when("kind", "many-to-one")
    .required(),
    Field::new(
        "destination",
        "Destination chat",
        Kind::reference(domains::CHAT, false),
    )
    .required(),
    Field::new(
        "only_chats",
        "Only these chats",
        Kind::reference(domains::CHAT, true),
    )
    .group("filter")
    .when("kind", "many-to-one"),
    Field::new(
        "except_chats",
        "Every chat except these",
        Kind::reference(domains::CHAT, true),
    )
    .group("filter")
    .when("kind", "many-to-one"),
];

/// Each connector's `owner_ids`, checked: no blank id and no id twice on one
/// connector.
fn owners(config: &Configuration) -> Result<HashMap<ConnectorId, HashSet<String>>, String> {
    let mut owners = HashMap::new();
    for connector in &config.connectors {
        let mut ids = HashSet::new();
        for id in &connector.owner_ids {
            if id.trim().is_empty() {
                return Err(format!(
                    "connector {}: owner_ids must not contain a blank id",
                    connector.id
                ));
            }
            if !ids.insert(id.clone()) {
                return Err(format!(
                    "connector {}: owner id {id:?} is listed twice",
                    connector.id
                ));
            }
        }
        owners.insert(ConnectorId(connector.id.clone()), ids);
    }
    Ok(owners)
}

/// The rules in match order: by `priority`, then by id. A map has no order of
/// its own, and the first matching rule wins.
fn rules(config: &Configuration) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    for (id, entry) in &config.rules {
        if !botconf::keys::is_entry_name(id) {
            return Err(format!(
                "rule id {id:?} must be non-empty and must not contain '.'"
            ));
        }
        if entry.id.is_some() {
            return Err(format!(
                "rule {id:?}: remove the id field; a rule's id is its key"
            ));
        }
        let flow = entry.flow.clone().into_rule_kind(id)?;
        rules.push((
            entry.priority,
            Rule {
                id: RuleId(id.clone()),
                flow,
            },
        ));
    }
    rules.sort_by(|(a_priority, a), (b_priority, b)| {
        a_priority.cmp(b_priority).then_with(|| a.id.0.cmp(&b.id.0))
    });
    Ok(rules.into_iter().map(|(_, rule)| rule).collect())
}

impl WuphfSchema {
    /// Every connector's token, failing when one is unset or empty, as
    /// provider tokens do.
    fn connector_tokens(
        &self,
        config: &Configuration,
    ) -> Result<HashMap<ConnectorId, String>, String> {
        let mut tokens = HashMap::new();
        for connector in &config.connectors {
            let Some(token) = (self.env)(&connector.token_env).filter(|token| !token.is_empty())
            else {
                return Err(format!(
                    "connector {} reads its token from {}, which is unset or empty",
                    connector.id, connector.token_env
                ));
            };
            tokens.insert(ConnectorId(connector.id.clone()), token);
        }
        Ok(tokens)
    }
}
