//! Core domain types shared across the wuphf workspace.
//!
//! wuphf replicates messages between chat platforms. The model is built around
//! a few ideas, from configured intent down to a single delivery:
//!
//! - [`Rule`] — a user-declared mapping between two or more [`RuleSide`]s. Rules
//!   are the only routing input a user writes; everything below is derived.
//! - [`SideKind`] — how one side selects chats: a fixed `Single` chat, a
//!   `Source` set of real chats, or a threaded `Mirror` container.
//! - [`Endpoint`] — a concrete destination (a chat, optionally a thread) on one
//!   connector, identified internally by an [`EndpointId`]. Platform addresses
//!   resolve to it through an [`Alias`], never by equality, so an address can
//!   migrate without changing identity.
//! - [`Conversation`] — the abstract unit a rule pairs across sides (one per
//!   source chat). Backed by one [`ReplicaSet`].
//! - [`ReplicaSet`] — the set of endpoints that mirror each other; a
//!   [`Message`] delivered to one is dispatched to all the others.
//! - [`Message`] — the normalized, platform-agnostic form copied out to every
//!   endpoint as a [`StoredDispatch`].
//!
//! Connectors are thin pipes: they report [`ConnectorInput`]s keyed by raw
//! platform addresses and execute [`ConnectorOutput`] dispatches. All routing
//! and identity resolution lives in the core.
//!
//! The types are grouped into submodules by domain ([`ids`], [`routing`],
//! [`connector`], [`message`], [`dispatch`]) and re-exported flatly, so callers
//! continue to refer to everything as `wuphf_model::Type`.

mod connector;
mod dispatch;
mod ids;
mod message;
mod observation;
mod routing;

pub use connector::*;
pub use dispatch::*;
pub use ids::*;
pub use message::*;
pub use observation::*;
pub use routing::*;
