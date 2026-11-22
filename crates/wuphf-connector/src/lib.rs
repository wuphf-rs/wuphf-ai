//! Shared client for wuphf connectors written in Rust: the counterpart of
//! `connectors/shared/coreclient` for the Go connectors. A connector supplies
//! a [`Sender`] implementation and a stream of inbound event frames; this
//! crate maintains the gRPC session against the core, uploads media to and
//! downloads it from the core's media store through [`MediaClient`], calls
//! the core's remote functions through [`FunctionsClient`], queues owner
//! notices with [`send_notice`], installs the log subscriber with a live level
//! ([`logging`]), and declares and follows runtime settings ([`settings`]).

mod client;
mod functions;
pub mod logging;
mod media;
mod notice;
mod sender;
pub mod settings;

pub use client::{Client, Options, PermanentFailureClassifier, SessionError};
pub use functions::{
    FunctionError, FunctionsClient, MEDIA_CONVERT, MEDIA_CONVERT_MAJOR, MediaConvertArgs,
    MediaConvertResult, MediaConverter,
};
pub use media::{MediaClient, MediaConnectError};
pub use notice::{notice, notice_with_media, send_notice};
pub use sender::{Delivery, Failure, Sender};
