//! Shared client for wuphf connectors written in Rust: the counterpart of
//! `connectors/shared/coreclient` for the Go connectors. A connector supplies
//! a [`Sender`] implementation and a stream of inbound event frames; this
//! crate maintains the gRPC session against the core, uploads media to the
//! core's media store through [`MediaClient`], and queues owner notices with
//! [`send_notice`].

mod client;
mod media;
mod notice;
mod sender;

pub use client::{Client, Options, PermanentFailureClassifier, SessionError};
pub use media::{MediaClient, MediaConnectError};
pub use notice::{notice, notice_with_media, send_notice};
pub use sender::{Delivery, Failure, Sender};
