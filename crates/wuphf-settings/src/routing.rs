//! The core's routing source over the settings store.

use crate::schema::SettingsStore;
use std::sync::Arc;
use wuphf_core::{RoutingConfig, RoutingSource};

/// The routing config of the settings store's current snapshot, so a change
/// applied to the store reaches the next event the core routes.
pub struct LiveRouting(Arc<SettingsStore>);

impl LiveRouting {
    pub fn new(store: Arc<SettingsStore>) -> Self {
        Self(store)
    }
}

impl RoutingSource for LiveRouting {
    fn routing(&self) -> Arc<RoutingConfig> {
        // An `ArcSwap` load: cheap and lock-free.
        Arc::clone(&self.0.current().derived.routing)
    }
}
