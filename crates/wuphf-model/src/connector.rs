//! Connector-facing types: advertised capabilities, registration, and the
//! frames exchanged between a connector and the core. How a connector renders
//! replicated operations (attachments-as-files, text spillover, edit/delete
//! rendering) is the connector's own configuration, not a core concern.

use crate::*;
use serde::{Deserialize, Serialize};

/// Capabilities a connector advertises at registration. A `Mirror` side is only
/// legal on a connector that supports both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorCapabilities {
    pub supports_threads: bool,
    pub supports_topic_create: bool,
}

/// A connector declared in core configuration. Endpoints are no longer
/// pre-assigned; ownership is derived from the rules that reference the
/// connector, and the connector advertises its capabilities at registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorRegistration {
    pub id: ConnectorId,
    pub platform: Platform,
    #[serde(default)]
    pub capabilities: ConnectorCapabilities,
}

/// A frame a connector sends up to the core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorInput {
    Event(NormalizedEvent),
    Acknowledgement(DispatchAcknowledgement),
    /// The platform reported that a chat address changed (e.g. WhatsApp
    /// phone-to-LID). Both addresses remain resolvable; the new one becomes
    /// canonical for outbound sends.
    IdentityMigrated {
        connector_id: ConnectorId,
        old: PlatformAddress,
        new: PlatformAddress,
    },
    /// The connector reported a dispatch can never succeed, so the core should
    /// dead-letter it instead of redelivering on the next session.
    DispatchFailed {
        dispatch_id: DispatchId,
        reason: String,
    },
}

/// A frame the core sends down to a connector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorOutput {
    /// A dispatch to execute, then acknowledge.
    Dispatch(Box<StoredDispatch>),
    /// The core refused the connector's last input (e.g. unknown registration).
    Rejected { reason: String },
}
