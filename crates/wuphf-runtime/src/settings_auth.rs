//! Who may use the core's settings service, and the names its panel shows.
//!
//! The settings store lives in the core, and the core decides who may change
//! it: connectors never decide, they pass along who acted (the `actor` of a
//! request) and the core checks it.
//!
//! - A connector authenticates with its own connector token (plan 034). Its
//!   request is allowed only when the actor's platform user id is one of
//!   **that** connector's `owner_ids`, so a connector cannot act for another
//!   connector's owner, and a request with no actor is refused, except that
//!   a connector may read and watch its own settings section
//!   (`connector_settings.<id>`) for itself, to apply them.
//! - The observer token (held by the TUI, which can already send messages as
//!   the owner) acts as the owner and needs no actor. With no observer token
//!   configured, nothing is accepted as the observer.
//!
//! [`WuphfNames`] names the references the settings hold: chats (from the
//! titles the core has stored), connectors and rules.

use crate::auth::{ConnectorTokens, bearer_token};
use crate::connector_commands::{COMMAND_TIMEOUT, ConnectorCommands};
use crate::connector_settings::section_connector;
use botconf_grpc::proto::Actor;
use botconf_grpc::proto::settings_server::SettingsServer;
use botconf_grpc::{Authorizer, Operation, Principal, SettingsService};
use botconf_panel::{Name, Names, Panel, SectionAction};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use tonic::Status;
use tonic::metadata::MetadataMap;
use tracing::warn;
use wuphf_model::{ChatAddress, ChatRef, ConnectorId};
use wuphf_settings::{PanelTokenStore, SettingsStore, WuphfSchema, domains};
use wuphf_storage::{ObservationStore, PanelTokens, StorageResult, TopologyStore};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The principal recorded for changes made with the observer token.
const OBSERVER: &str = "observer";

/// The command a platform's settings panel is opened with, for the panel's
/// texts (`/wuphf` on Telegram).
pub const PANEL_COMMAND: &str = "wuphf";

/// The core's `botconf.v1.Settings` service: the settings store and its
/// panel, whose long buttons' handles are kept in `storage` so they survive a
/// restart, behind [`WuphfAuthorizer`], with [`WuphfNames`] naming the
/// references. A connector's section also offers the commands its live
/// session declared (`commands`, plan 058).
pub fn settings_server<S>(
    settings: Arc<SettingsStore>,
    storage: Arc<S>,
    connectors: Arc<ConnectorTokens>,
    observer_token: Option<String>,
    commands: Arc<ConnectorCommands>,
) -> SettingsServer<SettingsService<WuphfSchema>>
where
    S: ObservationStore + TopologyStore + PanelTokens,
{
    let panel = domains::all().into_iter().fold(
        Panel::new(Arc::clone(&settings))
            .token_store(PanelTokenStore::new(Arc::clone(&storage)))
            .command(PANEL_COMMAND),
        Panel::domain,
    );
    let panel = with_commands(panel, commands);
    SettingsService::new(
        Arc::new(panel),
        WuphfAuthorizer::new(Arc::clone(&settings), connectors, observer_token),
    )
    .names(WuphfNames::new(storage, settings))
    .into_server()
}

/// Buttons on each connector's section for the commands its live session
/// declared, run on that session.
pub(crate) fn with_commands(
    panel: Panel<WuphfSchema>,
    commands: Arc<ConnectorCommands>,
) -> Panel<WuphfSchema> {
    let offered = Arc::clone(&commands);
    panel
        .section_actions(move |_, section| {
            section_connector(section)
                .map(|connector| offered.offered(connector))
                .unwrap_or_default()
                .into_iter()
                .map(|command| SectionAction {
                    id: command.id,
                    title: command.title,
                })
                .collect()
        })
        .on_action(move |call| {
            let commands = Arc::clone(&commands);
            Box::pin(async move {
                let connector = section_connector(&call.section)
                    .ok_or_else(|| format!("{} is not a connector's section", call.section))?;
                commands.run(connector, &call.action, COMMAND_TIMEOUT).await
            })
        })
}

/// Decides every settings request (see the module docs).
pub struct WuphfAuthorizer {
    store: Arc<SettingsStore>,
    connectors: Arc<ConnectorTokens>,
    /// `None` refuses the observer credential.
    observer_token: Option<String>,
}

impl WuphfAuthorizer {
    /// `observer_token` is the observer service's token; an empty one counts
    /// as none.
    pub fn new(
        store: Arc<SettingsStore>,
        connectors: Arc<ConnectorTokens>,
        observer_token: Option<String>,
    ) -> Self {
        Self {
            store,
            connectors,
            observer_token: observer_token.filter(|token| !token.is_empty()),
        }
    }
}

impl Authorizer for WuphfAuthorizer {
    fn authorize(
        &self,
        metadata: &MetadataMap,
        actor: Option<&Actor>,
        operation: Operation<'_>,
    ) -> Result<Principal, Status> {
        let Some(token) = bearer_token(metadata).filter(|token| !token.is_empty()) else {
            return Err(Status::unauthenticated("valid bearer token required"));
        };
        if self.observer_token.as_deref() == Some(token) {
            return Ok(Principal {
                actor: Some(OBSERVER.to_owned()),
            });
        }
        let Some(connector) = self.connectors.connector_for(token) else {
            return Err(Status::unauthenticated("valid bearer token required"));
        };
        // A connector may read and follow its own settings section for
        // itself, with no one acting: that is how it applies them (plan 040).
        if actor.is_none_or(|actor| actor.id.is_empty())
            && reads_only_own_section(connector, &operation)
        {
            return Ok(Principal {
                actor: Some(connector.0.clone()),
            });
        }
        let Some(actor) = actor.filter(|actor| !actor.id.is_empty()) else {
            return Err(Status::permission_denied(
                "a connector must say who is acting",
            ));
        };
        let snapshot = self.store.current();
        let is_owner = snapshot
            .derived
            .owners
            .get(connector)
            .is_some_and(|owners| owners.contains(&actor.id));
        if !is_owner {
            return Err(Status::permission_denied("not allowed"));
        }
        Ok(Principal {
            actor: Some(format!("{}:{}", connector.0, actor.id)),
        })
    }
}

/// Whether `operation` only reads `connector`'s own settings section: a
/// `Watch` or `Get` at or below `connector_settings.<id>`. An empty `Watch`
/// (every key) or a `List` reads more than that.
fn reads_only_own_section(connector: &ConnectorId, operation: &Operation<'_>) -> bool {
    let own = crate::connector_settings::section_path(connector);
    let is_own = |key: &str| {
        key == own
            || key
                .strip_prefix(own.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    };
    match operation {
        Operation::Watch { prefixes } => {
            !prefixes.is_empty() && prefixes.iter().all(|prefix| is_own(prefix))
        }
        Operation::Read { key: Some(key) } => is_own(key),
        _ => false,
    }
}

/// Names for the references the core's settings hold.
pub struct WuphfNames<S> {
    storage: Arc<S>,
    store: Arc<SettingsStore>,
}

impl<S> WuphfNames<S>
where
    S: ObservationStore + TopologyStore,
{
    pub fn new(storage: Arc<S>, store: Arc<SettingsStore>) -> Self {
        Self { storage, store }
    }

    /// The name of a chat reference: its chat's title, and for a thread, the
    /// title of the conversation the thread belongs to.
    async fn chat_name(&self, chat: &ChatRef) -> StorageResult<Option<String>> {
        let whole = ChatAddress::new(chat.address.chat_id.clone(), None);
        let chat_name = self.chat_title(&chat.connector_id, &whole).await?;
        if chat.address.thread_id.is_none() {
            return Ok(chat_name);
        }
        let thread_name = self
            .conversation_title(&chat.connector_id, &chat.address)
            .await?;
        Ok(match (chat_name, thread_name) {
            (Some(chat_name), Some(thread_name)) => Some(format!("{chat_name} › {thread_name}")),
            (Some(name), None) | (None, Some(name)) => Some(name),
            (None, None) => None,
        })
    }

    /// A chat's title: the observation log's, under this address or one of
    /// its linked forms, else the title of the conversation it is the source
    /// of.
    async fn chat_title(
        &self,
        connector: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<String>> {
        if let Some(title) = self.observed_title(connector, address).await? {
            return Ok(Some(title));
        }
        for linked in self.storage.linked_addresses(connector, address).await? {
            if let Some(title) = self.observed_title(connector, &linked).await? {
                return Ok(Some(title));
            }
        }
        self.conversation_title(connector, address).await
    }

    async fn observed_title(
        &self,
        connector: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<String>> {
        Ok(self
            .storage
            .observed_chat(connector, &address.chat_id)
            .await?
            .and_then(|chat| chat.title))
    }

    /// The title of the conversation the endpoint at `address` belongs to.
    async fn conversation_title(
        &self,
        connector: &ConnectorId,
        address: &ChatAddress,
    ) -> StorageResult<Option<String>> {
        let Some(endpoint) = self.storage.resolve_endpoint(connector, address).await? else {
            return Ok(None);
        };
        Ok(self
            .storage
            .conversation_for_endpoint(endpoint)
            .await?
            .and_then(|conversation| conversation.title))
    }

    /// `<id> (<platform>)`, as the configuration names the platform.
    fn connector_name(&self, id: &str) -> Option<String> {
        let snapshot = self.store.current();
        let connector = snapshot
            .config
            .connectors
            .iter()
            .find(|connector| connector.id == id)?;
        Some(format!("{id} ({})", connector.platform))
    }

    async fn chat_candidates(&self, query: &str, limit: usize) -> Vec<(Value, Name)> {
        let chats = match self.storage.list_observed_chats().await {
            Ok(chats) => chats,
            Err(error) => {
                warn!(%error, "settings panel: failed to list observed chats");
                return Vec::new();
            }
        };
        let snapshot = self.store.current();
        let configured = |id: &ConnectorId| {
            snapshot
                .derived
                .connectors
                .iter()
                .any(|connector| &connector.id == id)
        };
        let query = query.to_lowercase();
        chats
            .into_iter()
            .filter(|chat| configured(&chat.connector_id))
            .filter_map(|chat| {
                let id = ChatRef {
                    connector_id: chat.connector_id,
                    address: ChatAddress::new(chat.chat_id, None),
                }
                .to_string();
                let matches = query.is_empty()
                    || id.to_lowercase().contains(&query)
                    || chat
                        .title
                        .as_ref()
                        .is_some_and(|title| title.to_lowercase().contains(&query));
                matches.then(|| {
                    let label = chat.title.unwrap_or_else(|| id.clone());
                    (Value::String(id), name(label))
                })
            })
            .take(limit)
            .collect()
    }
}

fn name(label: String) -> Name {
    Name {
        short: label.clone(),
        full: label,
    }
}

impl<S> Names for WuphfNames<S>
where
    S: ObservationStore + TopologyStore,
{
    fn name<'a>(&'a self, domain: &'a str, id: &'a Value) -> BoxFuture<'a, Option<Name>> {
        Box::pin(async move {
            let id = id.as_str()?;
            match domain {
                domains::CONNECTOR => self.connector_name(id).map(name),
                // Rules are keyed by id; nothing nicer exists.
                domains::RULE => Some(name(id.to_owned())),
                domains::CHAT => {
                    let chat = ChatRef::from_str(id).ok()?;
                    match self.chat_name(&chat).await {
                        Ok(found) => found.map(name),
                        Err(error) => {
                            warn!(%error, chat = id, "settings panel: failed to name a chat");
                            None
                        }
                    }
                }
                _ => None,
            }
        })
    }

    fn candidates<'a>(
        &'a self,
        domain: &'a str,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Vec<(Value, Name)>> {
        Box::pin(async move {
            let snapshot = self.store.current();
            let query_lower = query.to_lowercase();
            let keep = |id: &str| query.is_empty() || id.to_lowercase().contains(&query_lower);
            match domain {
                domains::CHAT => self.chat_candidates(query, limit).await,
                // In configuration order.
                domains::CONNECTOR => snapshot
                    .config
                    .connectors
                    .iter()
                    .filter(|connector| keep(&connector.id))
                    .take(limit)
                    .map(|connector| {
                        let label = format!("{} ({})", connector.id, connector.platform);
                        (Value::String(connector.id.clone()), name(label))
                    })
                    .collect(),
                domains::RULE => {
                    let mut ids: Vec<&String> = snapshot.config.rules.keys().collect();
                    ids.sort();
                    ids.into_iter()
                        .filter(|id| keep(id))
                        .take(limit)
                        .map(|id| (Value::String(id.clone()), name(id.clone())))
                        .collect()
                }
                _ => Vec::new(),
            }
        })
    }
}

#[cfg(test)]
mod tests;
