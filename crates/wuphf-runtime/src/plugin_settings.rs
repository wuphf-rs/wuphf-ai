//! Plugins' own runtime settings (plan 078).
//!
//! A plugin is a client of the core that is neither a connector nor a
//! provider: wuphf-misc. It declares its runtime settings through
//! `PluginTransport.DeclareSettings` (see [`crate::declared_settings`]), with
//! the observer token. The core keeps them at `plugin_settings.<id>`, and the
//! plugin follows them through the settings service's `Watch`.
//!
//! The core does not know a plugin's features, so it does not list the keys a
//! plugin may declare, as it does for connectors; it refuses only the
//! client's connection keys ([`is_connection_key`]). Plugins are not in the
//! core's configuration, so every stored plugin declaration is restored.

use crate::declared_settings::{self, SectionSpec};
use botconf::DynamicSection;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::plugin_transport_server::PluginTransport;
use wuphf_settings::{PLUGIN_SETTINGS, SettingsStore};
use wuphf_storage::{SettingsDeclaration, SettingsDeclarations, SettingsOwner};

/// Where a plugin's settings live.
pub fn section_path(plugin: &str) -> String {
    format!("{PLUGIN_SETTINGS}.{plugin}")
}

const SECTION_PREFIX: &str = "plugin:";

/// Whether `id` can name a plugin: lowercase ASCII letters, digits, `-` and
/// `_`, starting with a letter.
pub fn is_valid_plugin_id(id: &str) -> bool {
    id.starts_with(|first: char| first.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Whether `key` is (or is below) one of the keys a plugin uses to reach the
/// core: its address, its id and the variables holding its tokens. Those are
/// read once, from the plugin's file.
fn is_connection_key(key: &str) -> bool {
    key.split('.').any(|part| {
        matches!(part, "core_address" | "plugin_id" | "token_env") || part.ends_with("_token_env")
    })
}

/// Registers plugins' settings sections in the core's settings store and
/// remembers their declarations.
pub struct PluginSettings {
    store: Arc<SettingsStore>,
    declarations: Arc<dyn SettingsDeclarations>,
}

impl PluginSettings {
    pub fn new(store: Arc<SettingsStore>, declarations: Arc<dyn SettingsDeclarations>) -> Self {
        Self {
            store,
            declarations,
        }
    }

    /// Registers the section of every stored plugin declaration, so stored
    /// overrides apply before plugins return.
    pub async fn restore(&self) -> Result<(), wuphf_storage::StorageError> {
        declared_settings::restore(&self.store, self.declarations.as_ref(), |declaration| {
            matches!(declaration.owner, SettingsOwner::Plugin(_)).then(|| section_for(declaration))
        })
        .await
    }

    /// Stores a plugin's declaration and registers (or replaces) its section,
    /// or says why it is refused.
    pub async fn declare(&self, declaration: SettingsDeclaration) -> Result<(), String> {
        let section = section_for(&declaration)?;
        declared_settings::declare(
            &self.store,
            self.declarations.as_ref(),
            &declaration,
            section,
        )
        .await
    }
}

/// The botconf section a plugin's declaration describes: its settings (no
/// connection keys), validated by its JSON Schema, over its own values.
pub fn section_for(declaration: &SettingsDeclaration) -> Result<DynamicSection, String> {
    let SettingsOwner::Plugin(plugin) = &declaration.owner else {
        return Err(format!("{} is not a plugin", declaration.owner.id()));
    };
    if !is_valid_plugin_id(plugin) {
        return Err(format!(
            "plugin id {plugin:?} must be lowercase letters, digits, '-' and '_', starting with \
             a letter"
        ));
    }
    declared_settings::declared_section(
        SectionSpec {
            id: format!("{SECTION_PREFIX}{plugin}"),
            path: section_path(plugin),
            title: format!("{plugin} (plugin)"),
            description: format!("The {plugin} plugin's own settings"),
            base_source: format!("plugin {plugin}"),
        },
        declaration,
        &|key| {
            if is_connection_key(key) {
                Err(format!(
                    "`{key}` cannot be a runtime setting: a plugin's address, id and token \
                     variables are read once, from its own file"
                ))
            } else {
                Ok(())
            }
        },
    )
}

/// The core's `PluginTransport` service. Put it behind the observer token
/// (`any_token_auth` with only that token, so no token refuses everything).
pub struct GrpcPluginService {
    settings: Arc<PluginSettings>,
}

impl GrpcPluginService {
    pub fn new(settings: Arc<PluginSettings>) -> Self {
        Self { settings }
    }
}

#[tonic::async_trait]
impl PluginTransport for GrpcPluginService {
    async fn declare_settings(
        &self,
        request: Request<pb::DeclarePluginSettingsRequest>,
    ) -> Result<Response<pb::DeclarePluginSettingsResponse>, Status> {
        let pb::DeclarePluginSettingsRequest {
            plugin_id,
            settings,
        } = request.into_inner();
        let settings =
            settings.ok_or_else(|| Status::invalid_argument("the declaration has no settings"))?;
        let declaration =
            declared_settings::declaration_from_wire(SettingsOwner::Plugin(plugin_id), settings);
        let plugin = declaration.owner.id().to_owned();
        self.settings.declare(declaration).await.map_err(|error| {
            Status::invalid_argument(format!("invalid settings declaration: {error}"))
        })?;
        tracing::info!(plugin, "plugin declared its settings");
        Ok(Response::new(pb::DeclarePluginSettingsResponse {}))
    }
}

#[cfg(test)]
mod tests;
