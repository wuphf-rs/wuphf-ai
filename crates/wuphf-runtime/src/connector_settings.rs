//! Connectors' own runtime settings (plan 040).
//!
//! A connector declares its runtime settings when it registers (see
//! [`crate::declared_settings`]). The core keeps them at
//! `connector_settings.<id>`, and the connector follows them through the
//! settings service's `Watch`.
//!
//! Only live-safe keys may be declared ([`LIVE_SAFE`]): a runtime setting
//! applies at once, so identity, addresses, session files and capabilities
//! stay in the connector's file.

use crate::declared_settings::{self, SectionSpec};
use botconf::DynamicSection;
use std::sync::Arc;
use wuphf_model::ConnectorId;
use wuphf_settings::{CONNECTOR_SETTINGS, SettingsStore};
use wuphf_storage::{SettingsDeclaration, SettingsDeclarations, SettingsOwner};

/// The keys a connector may declare, with everything below them: those that
/// can change while the connector runs.
pub const LIVE_SAFE: &[&str] = &[
    "rendering",
    "sticker_metadata",
    "log_level",
    "media_convert_timeout_ms",
];

/// Where a connector's settings live.
pub fn section_path(connector: &ConnectorId) -> String {
    format!("{CONNECTOR_SETTINGS}.{}", connector.0)
}

/// The id of a connector's section in the settings store.
fn section_id(connector: &ConnectorId) -> String {
    format!("{SECTION_PREFIX}{}", connector.0)
}

const SECTION_PREFIX: &str = "connector:";

/// The connector whose section `section` is, if it is one.
pub(crate) fn section_connector(section: &str) -> Option<&str> {
    section.strip_prefix(SECTION_PREFIX)
}

/// Registers connectors' settings sections in the core's settings store and
/// remembers their declarations.
pub struct ConnectorSettings {
    store: Arc<SettingsStore>,
    declarations: Arc<dyn SettingsDeclarations>,
}

impl ConnectorSettings {
    pub fn new(store: Arc<SettingsStore>, declarations: Arc<dyn SettingsDeclarations>) -> Self {
        Self {
            store,
            declarations,
        }
    }

    /// Registers the section of every stored connector declaration, so stored
    /// overrides apply before connectors return. A declaration that no longer
    /// fits (its connector left the configuration, or it no longer validates)
    /// is skipped with a warning; its overrides wait for the connector.
    pub async fn restore(&self) -> Result<(), wuphf_storage::StorageError> {
        declared_settings::restore(&self.store, self.declarations.as_ref(), |declaration| {
            matches!(declaration.owner, SettingsOwner::Connector(_))
                .then(|| self.section(declaration))
        })
        .await
    }

    /// The section a declaration describes, or why it is refused. Checked
    /// before a connector's session is accepted.
    pub fn section(&self, declaration: &SettingsDeclaration) -> Result<DynamicSection, String> {
        let SettingsOwner::Connector(connector) = &declaration.owner else {
            return Err(format!("{} is not a connector", declaration.owner.id()));
        };
        let snapshot = self.store.current();
        let platform = snapshot
            .config
            .connectors
            .iter()
            .find(|configured| configured.id == connector.0)
            .map(|configured| configured.platform.clone())
            .ok_or_else(|| format!("connector {} is not configured", connector.0))?;
        section_for(connector, &platform, declaration)
    }

    /// Stores a connector's declaration and registers (or replaces) its
    /// section. Call [`Self::section`] first to refuse a bad declaration
    /// before the session is accepted.
    pub async fn declare(&self, declaration: SettingsDeclaration) -> Result<(), String> {
        let section = self.section(&declaration)?;
        declared_settings::declare(
            &self.store,
            self.declarations.as_ref(),
            &declaration,
            section,
        )
        .await
    }
}

/// The botconf section a connector's declaration describes: its settings
/// (live-safe keys only), validated by its JSON Schema, over its own values.
pub fn section_for(
    connector: &ConnectorId,
    platform: &str,
    declaration: &SettingsDeclaration,
) -> Result<DynamicSection, String> {
    declared_settings::declared_section(
        SectionSpec {
            id: section_id(connector),
            path: section_path(connector),
            title: format!("{} ({platform})", connector.0),
            description: format!("The {} connector's own settings", connector.0),
            base_source: format!("connector {}", connector.0),
        },
        declaration,
        &|key| {
            if is_live_safe(key) {
                Ok(())
            } else {
                Err(format!(
                    "`{key}` cannot be a runtime setting: only {} (and keys below them) can \
                     change while a connector runs",
                    LIVE_SAFE.join(", "),
                ))
            }
        },
    )
}

/// Whether `key` is one of [`LIVE_SAFE`] or below one.
fn is_live_safe(key: &str) -> bool {
    LIVE_SAFE.iter().any(|safe| {
        key == *safe
            || key
                .strip_prefix(safe)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// A declaration from a registration frame.
pub(crate) fn declaration_from_wire(
    connector: &ConnectorId,
    declaration: wuphf_proto::connector::ConnectorSettingsDeclaration,
) -> SettingsDeclaration {
    declared_settings::declaration_from_wire(
        SettingsOwner::Connector(connector.clone()),
        declaration,
    )
}

#[cfg(test)]
mod tests;
