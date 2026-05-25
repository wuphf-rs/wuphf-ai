//! Compiled routing and endpoint-ownership lookup tables.

use crate::config::{Config, EndpointConfig};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug)]
/// Read-only topology indexes used while processing inbound messages.
pub(crate) struct RoutingTable {
    /// Connector owner keyed by endpoint ID.
    endpoint_owners: HashMap<String, String>,
    /// Deterministic distinct fan-out destinations keyed by origin endpoint ID.
    targets: HashMap<String, Vec<EndpointConfig>>,
}

impl RoutingTable {
    /// Compiles endpoint ownership and destinations from validated configuration.
    pub(crate) fn from_config(config: &Config) -> Self {
        let endpoint_owners = config
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.id.clone(), endpoint.connector_id.clone()))
            .collect();
        let endpoints: HashMap<_, _> = config
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.id.as_str(), endpoint))
            .collect();
        let targets = config
            .endpoints
            .iter()
            .map(|origin| {
                let target_ids: BTreeSet<_> = config
                    .replication_groups
                    .iter()
                    .filter(|group| group.endpoints.contains(&origin.id))
                    .flat_map(|group| group.endpoints.iter())
                    .filter(|endpoint_id| *endpoint_id != &origin.id)
                    .collect();
                let targets = target_ids
                    .into_iter()
                    .filter_map(|id| endpoints.get(id.as_str()))
                    .map(|endpoint| (*endpoint).clone())
                    .collect();
                (origin.id.clone(), targets)
            })
            .collect();
        Self {
            endpoint_owners,
            targets,
        }
    }

    /// Reports whether a connector may originate events for an endpoint.
    pub(crate) fn connector_owns_endpoint(&self, connector_id: &str, endpoint_id: &str) -> bool {
        self.endpoint_owners
            .get(endpoint_id)
            .is_some_and(|owner| owner == connector_id)
    }

    /// Returns deterministic distinct replication targets for an origin endpoint.
    pub(crate) fn target_endpoints(&self, origin_endpoint_id: &str) -> &[EndpointConfig] {
        self.targets
            .get(origin_endpoint_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
/// Unit tests for topology compilation behavior.
mod tests {
    use super::*;

    #[test]
    /// Collapses duplicate fan-out membership introduced by overlapping groups.
    fn overlapping_groups_are_compiled_into_distinct_targets() {
        let config: Config = toml::from_str(
            r#"
            [server]
            listen = "localhost:1"
            [database]
            url = "sqlite::memory:"
            [[connectors]]
            id = "a"
            token = "a"
            [[connectors]]
            id = "b"
            token = "b"
            [[connectors]]
            id = "c"
            token = "c"
            [[endpoints]]
            id = "a-room"
            connector_id = "a"
            address = "a"
            [[endpoints]]
            id = "b-room"
            connector_id = "b"
            address = "b"
            [[endpoints]]
            id = "c-room"
            connector_id = "c"
            address = "c"
            [[replication_groups]]
            id = "all"
            endpoints = ["a-room", "b-room", "c-room"]
            [[replication_groups]]
            id = "repeat"
            endpoints = ["a-room", "b-room"]
            "#,
        )
        .unwrap();
        let table = RoutingTable::from_config(&config);

        let targets: Vec<_> = table
            .target_endpoints("a-room")
            .iter()
            .map(|endpoint| endpoint.id.as_str())
            .collect();
        assert_eq!(targets, vec!["b-room", "c-room"]);
        assert!(table.connector_owns_endpoint("a", "a-room"));
    }
}
