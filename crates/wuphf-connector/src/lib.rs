//! Shared client for wuphf connectors written in Rust: the counterpart of
//! `connectors/shared/coreclient` for the Go connectors. A connector supplies
//! a [`Sender`] implementation and a stream of inbound event frames; this
//! crate maintains the gRPC session against the core.

mod client;
mod media;
mod sender;

pub use client::{Client, Options, PermanentFailureClassifier, SessionError};
pub use media::{MediaClient, MediaConnectError};
pub use sender::{Delivery, Failure, Sender};
