//! Core domain types shared across the wuphf workspace.
//!
//! wuphf replicates messages between chat platforms. The model is built around
//! a few ideas, from configured intent down to a single delivery:
//!
//! - [`Rule`] — a user-declared flow between a source end and a
//!   destination end ([`RuleKind`]: one-to-one or many-to-one). Rules
//!   are the only routing input a user writes; everything below is derived.
//! - [`Endpoint`] — a concrete destination (a chat, optionally a thread) on one
//!   connector, identified internally by an [`EndpointId`]. Platform addresses
//!   resolve to it through an [`Alias`], never by equality, so an address can
//!   migrate without changing identity.
//! - [`Conversation`] — the chats that mirror each other, one per source chat a
//!   rule matches; a [`Message`] delivered to one endpoint is dispatched to all
//!   the others.
//! - [`Message`] — the normalized, platform-agnostic form copied out to every
//!   endpoint as a [`Dispatch`].
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
