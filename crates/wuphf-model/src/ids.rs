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
    "Id of one outbound [`Dispatch`] targeting a single endpoint."
);
uuid_id!(
    MediaId,
    "Id of a stored media blob referenced by an [`Attachment`]."
);
uuid_id!(
    ConversationId,
    "Id of a [`Conversation`] — the unit a [`Rule`] pairs across its ends."
);

// User-chosen string ids have no `new()`/`Default`: they come from config or a
// connector registration, never minted at runtime.
macro_rules! string_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

string_id!(
    RuleId,
    "User-chosen id of a [`Rule`], taken verbatim from configuration."
);
string_id!(
    ConnectorId,
    "Stable id of a connector, matching a `[[connector]]` entry in core config and the registration a connector sends on connect."
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Both string ids are persisted inside JSON payloads (sqlite) — they must
    /// keep serializing as bare strings.
    #[test]
    fn string_ids_serde_as_bare_json_strings() {
        let connector = ConnectorId("whatsapp".into());
        let rule = RuleId("family".into());
        assert_eq!(serde_json::to_string(&connector).unwrap(), r#""whatsapp""#);
        assert_eq!(serde_json::to_string(&rule).unwrap(), r#""family""#);
        assert_eq!(
            serde_json::from_str::<ConnectorId>(r#""whatsapp""#).unwrap(),
            connector
        );
        assert_eq!(serde_json::from_str::<RuleId>(r#""family""#).unwrap(), rule);
    }
}
