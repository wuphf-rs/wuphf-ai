//! Identifier newtypes. Internal identities are random UUIDs minted at runtime;
//! [`RuleId`] and [`ConnectorId`] are user-chosen strings taken from config.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

uuid_id!(
    MessageId,
    "Stable internal id of a normalized [`Message`]. Native platform message ids map to it via [`MessageCopy`]."
);
uuid_id!(
    ReplicaSetId,
    "Id of a [`ReplicaSet`] — the set of endpoints a message fans out to."
);
uuid_id!(
    EndpointId,
    "Stable internal id of an [`Endpoint`]. Platform addresses resolve to it through [`Alias`]es, never by equality."
);
uuid_id!(
    DispatchId,
    "Id of one outbound [`StoredDispatch`] targeting a single endpoint."
);
uuid_id!(
    MediaId,
    "Id of a stored media blob referenced by an [`Attachment`]."
);
uuid_id!(
    ConversationId,
    "Id of a [`Conversation`] — the unit a [`Rule`] pairs across its sides."
);

/// User-chosen id of a routing [`Rule`], taken verbatim from configuration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Stable id of a connector, matching a `[[connector]]` entry in core config and
/// the registration a connector sends on connect.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectorId(pub String);

impl fmt::Display for ConnectorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
