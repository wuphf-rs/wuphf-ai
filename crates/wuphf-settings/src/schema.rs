//! The schema: how botconf validates the configuration and what it derives
//! from it.

use crate::config::{Configuration, platform};
use botconf::{Kind, RuntimeSetting};
use std::collections::HashMap;
use std::sync::Arc;
use wuphf_core::RoutingConfig;
use wuphf_functions::Functions;
use wuphf_model::{
    Connector, ConnectorCapabilities, ConnectorId, Rule, RuleId, is_valid_connector_id,
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
}

/// Reads an environment variable by name.
type EnvReader = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Describes the core configuration to botconf.
pub struct WuphfSchema {
    /// Reads environment variables (tokens); injectable for tests.
    env: Arc<EnvReader>,
}

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
                platform: platform(&connector.platform)?,
                capabilities: ConnectorCapabilities {
                    supports_threads: connector.supports_threads,
                    supports_topic_create: connector.supports_topic_create,
                },
            });
        }
        let connector_tokens = self.connector_tokens(config)?;
        let notice_targets = config
            .notify
            .iter()
            .map(|target| target.0.clone())
            .collect();
        let routing = RoutingConfig::new(rules(config)?, connectors.clone())
            .with_notice_targets(notice_targets);
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
        })
    }

    /// The keys that may change while the core runs. Everything else (the
    /// connectors, `listen`, `database_url`, the tokens, `functions`) is read
    /// once at startup, so it stays file and environment only.
    fn settings(&self) -> Vec<RuntimeSetting> {
        vec![
            RuntimeSetting::per_entry(
                "rules",
                "Routing rules by id: which chats mirror each other. Lower priority numbers match first.",
                &Kind::Json,
            )
            .titled("Rules"),
            RuntimeSetting::new(
                "notify",
                "Chats that receive notices about connector trouble",
            )
            .titled("Notice targets")
            .kind(Kind::Json),
        ]
    }
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
