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

/// A connector's report that a dispatch can never succeed, so the core should
/// dead-letter it instead of redelivering on the next session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchFailed {
    pub dispatch_id: DispatchId,
    pub reason: String,
}

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

/// Ordered inbound work from one connector: a normalized event, an identity
/// migration or a health notice. All travel one per-connector path so the
/// connector's frame order is preserved end to end. A migration that precedes
/// a message is applied before that message is ingested, so the message cannot
/// mint a fresh endpoint under an address the migration was meant to rebind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboundWork {
    Event(Event),
    /// The platform reported that a chat address changed (e.g. WhatsApp
    /// phone-to-LID). Both addresses remain resolvable; the new one becomes
    /// canonical for outbound sends.
    Migration {
        connector_id: ConnectorId,
        old: ChatAddress,
        new: ChatAddress,
    },
    /// The connector reported on its own health (e.g. its session was ended by
    /// the platform), for the core to relay to the owner through the other
    /// connectors. It shares the queue so a notice lands in the order the
    /// connector raised it, after whatever it had already reported.
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

    /// Stamp the connector the session belongs to. The session, not the wire,
    /// is the authority on which connector spoke, so a connector cannot present
    /// itself as another.
    pub fn set_connector(&mut self, id: ConnectorId) {
        match self {
            InboundWork::Event(event) => event.source.connector_id = id,
            InboundWork::Migration { connector_id, .. }
            | InboundWork::Notice { connector_id, .. } => *connector_id = id,
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
    /// An event, identity migration or health notice, applied in connector
    /// order.
    Work(Box<InboundWork>),
    Acknowledgement(DispatchAck),
    DispatchFailed(DispatchFailed),
}

impl ConnectorInput {
    /// A normalized event, the most common frame a connector sends.
    pub fn event(event: Event) -> Self {
        Self::Work(Box::new(InboundWork::Event(event)))
    }
}

/// A frame the core sends down to a connector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorOutput {
    /// A dispatch to execute, then acknowledge.
    Dispatch(Box<Dispatch>),
    /// The core refused the connector's last input (e.g. unknown registration).
    Rejected { reason: String },
}
