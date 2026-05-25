use serde::Deserialize;
use std::{collections::HashSet, fs, path::Path};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub connectors: Vec<ConnectorConfig>,
    pub endpoints: Vec<EndpointConfig>,
    pub replication_groups: Vec<ReplicationGroupConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServerConfig {
    pub listen: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConnectorConfig {
    pub id: String,
    pub token: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EndpointConfig {
    pub id: String,
    pub connector_id: String,
    pub address: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReplicationGroupConfig {
    pub id: String,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read configuration: {0}")]
    Read(#[from] std::io::Error),
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid topology: {0}")]
    Validation(String),
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let value = toml::from_str::<Self>(&fs::read_to_string(path)?)?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.listen.trim().is_empty() || self.database.url.trim().is_empty() {
            return Err(ConfigError::Validation(
                "server listen address and database URL are required".into(),
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

    pub fn connector_token(&self, id: &str) -> Option<&str> {
        self.connectors
            .iter()
            .find(|connector| connector.id == id)
            .map(|connector| connector.token.as_str())
    }

    pub fn connector_owns_endpoint(&self, connector_id: &str, endpoint_id: &str) -> bool {
        self.endpoints
            .iter()
            .any(|endpoint| endpoint.id == endpoint_id && endpoint.connector_id == connector_id)
    }

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
mod tests {
    use super::*;

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
    fn accepts_a_valid_topology() {
        valid().validate().unwrap();
    }

    #[test]
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
}
