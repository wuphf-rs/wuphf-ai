//! TOML configuration models and topology validation.

use serde::Deserialize;
use std::{collections::HashSet, fs, path::Path};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
/// Complete core service configuration loaded from TOML.
pub struct Config {
    /// Network listener settings for the core gRPC server.
    pub server: ServerConfig,
    /// Persistent database connection settings.
    pub database: DatabaseConfig,
    #[serde(default)]
    /// Durable dispatch delivery and retry timing policy.
    pub delivery: DeliveryConfig,
    /// Connector instances authorized to establish streams.
    pub connectors: Vec<ConnectorConfig>,
    /// Addressable endpoints owned by connector instances.
    pub endpoints: Vec<EndpointConfig>,
    /// Static groups defining endpoint fan-out membership.
    pub replication_groups: Vec<ReplicationGroupConfig>,
}

#[derive(Clone, Debug, Deserialize)]
/// Network settings for the core server.
pub struct ServerConfig {
    /// Socket address on which the gRPC server listens.
    pub listen: String,
}

#[derive(Clone, Debug, Deserialize)]
/// SQLite connection settings for durable state.
pub struct DatabaseConfig {
    /// SQLx SQLite connection URL.
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
/// Timing controls for dispatch leases and transient retries.
pub struct DeliveryConfig {
    /// Milliseconds allowed for acknowledgement before an in-flight dispatch is retryable.
    pub acknowledgement_timeout_ms: u64,
    /// Initial delay in milliseconds after a transient failure.
    pub retry_initial_ms: u64,
    /// Maximum exponential retry delay in milliseconds.
    pub retry_max_ms: u64,
}

impl Default for DeliveryConfig {
    /// Returns conservative default delivery timing suitable for local and initial operation.
    fn default() -> Self {
        Self {
            acknowledgement_timeout_ms: 30_000,
            retry_initial_ms: 1_000,
            retry_max_ms: 60_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
/// Authentication configuration for one connector instance.
pub struct ConnectorConfig {
    /// Stable connector identity supplied during stream registration.
    pub id: String,
    /// Shared registration secret for the connector instance.
    pub token: String,
}

#[derive(Clone, Debug, Deserialize)]
/// Configured origin or destination controlled by a connector.
pub struct EndpointConfig {
    /// Stable core-owned endpoint identity.
    pub id: String,
    /// Identifier of the connector responsible for this endpoint.
    pub connector_id: String,
    /// Connector-interpreted address for the endpoint.
    pub address: String,
}

#[derive(Clone, Debug, Deserialize)]
/// Static collection of endpoints among which messages replicate.
pub struct ReplicationGroupConfig {
    /// Stable replication group identity.
    pub id: String,
    /// IDs of endpoints participating in the group.
    pub endpoints: Vec<String>,
}

#[derive(Debug, Error)]
/// Failure while loading or validating runtime configuration.
pub enum ConfigError {
    /// Configuration file could not be read.
    #[error("cannot read configuration: {0}")]
    Read(#[from] std::io::Error),
    /// Configuration text was not valid TOML.
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    /// Parsed configuration violated a topology or policy invariant.
    #[error("invalid topology: {0}")]
    Validation(String),
}

impl Config {
    /// Loads TOML from `path` and validates all topology and timing invariants.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let value = toml::from_str::<Self>(&fs::read_to_string(path)?)?;
        value.validate()?;
        Ok(value)
    }

    /// Validates references, identities, authentication values, and delivery timings.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.listen.trim().is_empty() || self.database.url.trim().is_empty() {
            return Err(ConfigError::Validation(
                "server listen address and database URL are required".into(),
            ));
        }
        if self.delivery.acknowledgement_timeout_ms == 0
            || self.delivery.retry_initial_ms == 0
            || self.delivery.retry_max_ms < self.delivery.retry_initial_ms
        {
            return Err(ConfigError::Validation(
                "delivery timeouts must be positive and retry_max_ms must be at least retry_initial_ms"
                    .into(),
            ));
        }

        let mut connector_ids = HashSet::new();
        for connector in &self.connectors {
            if connector.id.trim().is_empty() || connector.token.trim().is_empty() {
                return Err(ConfigError::Validation(
                    "connector IDs and shared tokens must not be empty".into(),
                ));
            }
            if !connector_ids.insert(connector.id.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "duplicate connector ID '{}'",
                    connector.id
                )));
            }
        }

        let mut endpoint_ids = HashSet::new();
        for endpoint in &self.endpoints {
            if endpoint.id.trim().is_empty() || endpoint.address.trim().is_empty() {
                return Err(ConfigError::Validation(
                    "endpoint IDs and addresses must not be empty".into(),
                ));
            }
            if !endpoint_ids.insert(endpoint.id.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "duplicate endpoint ID '{}'",
                    endpoint.id
                )));
            }
            if !connector_ids.contains(endpoint.connector_id.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "endpoint '{}' refers to unknown connector '{}'",
                    endpoint.id, endpoint.connector_id
                )));
            }
        }

        let mut group_ids = HashSet::new();
        for group in &self.replication_groups {
            if group.id.trim().is_empty() || !group_ids.insert(group.id.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "missing or duplicate replication group ID '{}'",
                    group.id
                )));
            }
            let distinct: HashSet<_> = group.endpoints.iter().collect();
            if distinct.len() < 2 || distinct.len() != group.endpoints.len() {
                return Err(ConfigError::Validation(format!(
                    "replication group '{}' must contain at least two distinct endpoints",
                    group.id
                )));
            }
            for endpoint in &group.endpoints {
                if !endpoint_ids.contains(endpoint.as_str()) {
                    return Err(ConfigError::Validation(format!(
                        "replication group '{}' refers to unknown endpoint '{}'",
                        group.id, endpoint
                    )));
                }
            }
        }
        Ok(())
    }

    /// Returns the configured shared token for a connector identity.
    pub fn connector_token(&self, id: &str) -> Option<&str> {
        self.connectors
            .iter()
            .find(|connector| connector.id == id)
            .map(|connector| connector.token.as_str())
    }

    /// Reports whether `connector_id` is configured as owner of `endpoint_id`.
    pub fn connector_owns_endpoint(&self, connector_id: &str, endpoint_id: &str) -> bool {
        self.endpoints
            .iter()
            .any(|endpoint| endpoint.id == endpoint_id && endpoint.connector_id == connector_id)
    }

    /// Computes all distinct fan-out endpoints reachable from an origin endpoint.
    pub fn target_endpoints(&self, origin_endpoint: &str) -> Vec<&EndpointConfig> {
        let target_ids: std::collections::BTreeSet<_> = self
            .replication_groups
            .iter()
            .filter(|group| group.endpoints.iter().any(|id| id == origin_endpoint))
            .flat_map(|group| group.endpoints.iter())
            .filter(|id| id.as_str() != origin_endpoint)
            .collect();

        target_ids
            .into_iter()
            .filter_map(|id| self.endpoints.iter().find(|endpoint| endpoint.id == *id))
            .collect()
    }
}

#[cfg(test)]
/// Unit tests for topology and delivery-policy validation.
mod tests {
    use super::*;

    /// Builds a minimal valid two-endpoint configuration for mutation in tests.
    fn valid() -> Config {
        toml::from_str(
            r#"
                [server]
                listen = "127.0.0.1:1"
                [database]
                url = "sqlite::memory:"
                [[connectors]]
                id = "a"
                token = "token"
                [[connectors]]
                id = "b"
                token = "token"
                [[endpoints]]
                id = "one"
                connector_id = "a"
                address = "one"
                [[endpoints]]
                id = "two"
                connector_id = "b"
                address = "two"
                [[replication_groups]]
                id = "group"
                endpoints = ["one", "two"]
            "#,
        )
        .unwrap()
    }

    #[test]
    /// Accepts a complete valid topology.
    fn accepts_a_valid_topology() {
        valid().validate().unwrap();
    }

    #[test]
    /// Rejects identity duplication and dangling topology references.
    fn rejects_duplicate_or_unknown_topology_references() {
        let mut config = valid();
        config.connectors.push(config.connectors[0].clone());
        assert!(config.validate().is_err());

        let mut config = valid();
        config.replication_groups[0]
            .endpoints
            .push("missing".into());
        assert!(config.validate().is_err());
    }

    #[test]
    /// Rejects an invalid exponential retry timing range.
    fn rejects_invalid_delivery_retry_window() {
        let mut config = valid();
        config.delivery.retry_initial_ms = 10;
        config.delivery.retry_max_ms = 1;
        assert!(config.validate().is_err());
    }
}
