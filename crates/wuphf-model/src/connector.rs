//! Connector-facing types: advertised capabilities, server-owned delivery
//! policy, registration, and the frames exchanged between a connector and the
//! core.

use crate::*;
use serde::{Deserialize, Serialize};

/// Capabilities a connector advertises at registration. A `Mirror` side is only
/// legal on a connector that supports both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorCapabilities {
    pub supports_threads: bool,
    pub supports_topic_create: bool,
}

/// How a connector renders a replicated edit onto an already-delivered copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditMode {
    /// Revise the original copy in place.
    #[default]
    InPlace,
    /// Leave the original copy untouched and post the revision as a reply
    /// marked as an edit. For platforms where editing is undesirable or where
    /// an edit notification is wanted.
    Repost,
}

/// How a connector renders a replicated delete onto an already-delivered copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    /// Delete the mirrored copy.
    #[default]
    Revoke,
    /// Leave the copy in place and post a notice replying to it.
    Tombstone,
}

/// Per-connector delivery policy: server-owned options that govern how a
/// connector renders replicated operations on its platform. Configured on each
/// connector and resolved against the *target* connector when routing, so policy
/// is connector-specific and directional (per delivery destination) rather than
/// a symmetric property of a replica set. Each option is independent and
/// defaulted, so a connector applies the knobs its platform supports and ignores
/// the rest; new options are added here as additive fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeliveryOptions {
    pub edit_mode: EditMode,
    pub delete_mode: DeleteMode,
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
