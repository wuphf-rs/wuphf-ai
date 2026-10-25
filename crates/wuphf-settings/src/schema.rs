//! The schema: how botconf validates the configuration and what it derives
//! from it.

use crate::config::{Configuration, platform};
use std::collections::HashMap;
use std::sync::Arc;
use wuphf_core::RoutingConfig;
use wuphf_functions::Functions;
use wuphf_model::{Connector, ConnectorCapabilities, ConnectorId};

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
        let routing = RoutingConfig::new(config.rules.clone(), connectors.clone())
            .with_notice_targets(config.notify.clone());
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

    fn settings(&self) -> Vec<botconf::RuntimeSetting> {
        // No key changes at runtime yet; plan 037 adds `rules` and `notify`.
        Vec::new()
    }
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
