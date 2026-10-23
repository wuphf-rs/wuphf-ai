//! Connector-facing types: advertised capabilities, registration, and the
//! frames exchanged between a connector and the core. How a connector renders
//! replicated operations (attachments-as-files, text spillover, edit/delete
//! rendering) is the connector's own configuration, not a core concern.

use crate::*;
use serde::{Deserialize, Serialize};

/// Capabilities a connector advertises at registration. A many-to-one
/// destination is only legal on a connector that supports both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorCapabilities {
    pub supports_threads: bool,
    pub supports_topic_create: bool,
}

/// A terminal dispatch failure reported by a connector: `(dispatch, reason)`.
pub type DispatchFailure = (DispatchId, String);

/// How badly a connector's own operation is affected, as the connector reports
/// it. The core turns this into the wording the owner reads, so a connector
/// never decides how its trouble is phrased.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// The platform ended the session; it cannot recover without the owner
    /// re-authenticating (a WhatsApp logout, a revoked pairing).
    LoggedOut,
    /// Credentials were rejected or expired, and the connector needs new ones.
    AuthRequired,
    /// Still running, but impaired — rate limited, temporarily banned, or
    /// otherwise degraded.
    Degraded,
    /// Back to normal after one of the above.
    Recovered,
}

impl NoticeKind {
    /// One line describing the state, used to compose the owner's message.
    pub fn summary(self) -> &'static str {
        match self {
            NoticeKind::LoggedOut => {
                "logged out and is no longer bridging; re-pair it to restore the link"
            }
            NoticeKind::AuthRequired => "needs new credentials before it can bridge again",
            NoticeKind::Degraded => "is degraded and may be dropping or delaying messages",
            NoticeKind::Recovered => "is working again",
        }
    }
}

/// Where an owner notice is delivered: a chat on a connector that is expected to
/// still be working. Several may be configured, since the point is to reach the
/// owner somewhere other than the connector in trouble.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeTarget {
    pub connector_id: ConnectorId,
    pub chat_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

/// Ordered inbound work from one connector: a normalized event or an identity
/// migration. Both travel one per-connector path so the connector's frame order
/// is preserved end to end. A migration that precedes a message is applied
/// before that message is ingested, so the message cannot mint a fresh endpoint
/// under an address the migration was meant to rebind.
pub enum InboundWork {
    Event(Event),
    Migration {
        connector_id: ConnectorId,
        old: ChatAddress,
        new: ChatAddress,
    },
    /// The connector reported on its own health. It shares the queue so a
    /// notice lands in the order the connector raised it, after whatever it had
    /// already reported.
    Notice {
        connector_id: ConnectorId,
        kind: NoticeKind,
        detail: String,
    },
}

impl InboundWork {
    /// The connector this work arrived from, keying its ordered worker queue.
    pub fn connector_id(&self) -> &ConnectorId {
        match self {
            InboundWork::Event(event) => &event.source.connector_id,
            InboundWork::Migration { connector_id, .. } => connector_id,
            InboundWork::Notice { connector_id, .. } => connector_id,
        }
    }
}

/// A connector declared in core configuration. Endpoints are no longer
/// pre-assigned; ownership is derived from the rules that reference the
/// connector, and the connector advertises its capabilities at registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connector {
    pub id: ConnectorId,
    pub platform: Platform,
    #[serde(default)]
    pub capabilities: ConnectorCapabilities,
}

/// A frame a connector sends up to the core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorInput {
    Event(Event),
    Acknowledgement(DispatchAck),
    /// The platform reported that a chat address changed (e.g. WhatsApp
    /// phone-to-LID). Both addresses remain resolvable; the new one becomes
    /// canonical for outbound sends.
    IdentityMigrated {
        connector_id: ConnectorId,
        old: ChatAddress,
        new: ChatAddress,
    },
    /// The connector reported a dispatch can never succeed, so the core should
    /// dead-letter it instead of redelivering on the next session.
    DispatchFailed {
        dispatch_id: DispatchId,
        reason: String,
    },
    /// The connector reported on its own health (e.g. its session was ended by
    /// the platform), for the core to relay to the owner through the other
    /// connectors.
    OwnerNotice {
        connector_id: ConnectorId,
        kind: NoticeKind,
        detail: String,
    },
}

/// A frame the core sends down to a connector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorOutput {
    /// A dispatch to execute, then acknowledge.
    Dispatch(Box<Dispatch>),
    /// The core refused the connector's last input (e.g. unknown registration).
    Rejected { reason: String },
}
