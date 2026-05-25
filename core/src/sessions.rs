//! Single-active-stream registry for authenticated connector instances.

use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, Mutex};
use tonic::Status;
use uuid::Uuid;
use wuphf_proto::v1::CoreToConnector;

/// Sender used to write protocol messages or terminal stream statuses to a connector.
pub(crate) type SessionSender = mpsc::Sender<Result<CoreToConnector, Status>>;

#[derive(Clone, Default)]
/// Concurrent registry mapping each connector identity to its current stream.
pub(crate) struct SessionRegistry {
    /// Live connector streams guarded for concurrent service tasks.
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

/// One currently active connector stream registration.
struct Session {
    /// Unique registration generation used to reject replaced streams.
    id: String,
    /// Outbound channel for the registered stream.
    sender: SessionSender,
}

/// Result of registering a connector stream.
pub(crate) struct RegisteredSession {
    /// Unique ID for the newly active session.
    pub(crate) id: String,
    /// Prior stream sender to abort when registration replaced an active session.
    pub(crate) replaced_sender: Option<SessionSender>,
}

impl SessionRegistry {
    /// Installs a connector stream as active and returns any replaced stream sender.
    pub(crate) async fn register(
        &self,
        connector_id: String,
        sender: SessionSender,
    ) -> RegisteredSession {
        let id = Uuid::new_v4().to_string();
        let replaced_sender = self
            .sessions
            .lock()
            .await
            .insert(
                connector_id,
                Session {
                    id: id.clone(),
                    sender,
                },
            )
            .map(|session| session.sender);
        RegisteredSession {
            id,
            replaced_sender,
        }
    }

    /// Returns outbound sender for current active connector stream, when online.
    pub(crate) async fn sender(&self, connector_id: &str) -> Option<SessionSender> {
        self.sessions
            .lock()
            .await
            .get(connector_id)
            .map(|session| session.sender.clone())
    }

    /// Checks whether a session generation still owns connector identity.
    pub(crate) async fn is_active(&self, connector_id: &str, session_id: &str) -> bool {
        self.sessions
            .lock()
            .await
            .get(connector_id)
            .is_some_and(|session| session.id == session_id)
    }

    /// Removes connector stream only if caller still owns active session generation.
    pub(crate) async fn remove_if_active(&self, connector_id: &str, session_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(connector_id)
            .is_some_and(|session| session.id == session_id)
        {
            sessions.remove(connector_id);
        }
    }
}
