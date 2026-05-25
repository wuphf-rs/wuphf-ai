use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, Mutex};
use tonic::Status;
use uuid::Uuid;
use wuphf_proto::v1::CoreToConnector;

pub(crate) type SessionSender = mpsc::Sender<Result<CoreToConnector, Status>>;

#[derive(Clone, Default)]
pub(crate) struct SessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

struct Session {
    id: String,
    sender: SessionSender,
}

pub(crate) struct RegisteredSession {
    pub(crate) id: String,
    pub(crate) replaced_sender: Option<SessionSender>,
}

impl SessionRegistry {
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

    pub(crate) async fn sender(&self, connector_id: &str) -> Option<SessionSender> {
        self.sessions
            .lock()
            .await
            .get(connector_id)
            .map(|session| session.sender.clone())
    }

    pub(crate) async fn is_active(&self, connector_id: &str, session_id: &str) -> bool {
        self.sessions
            .lock()
            .await
            .get(connector_id)
            .is_some_and(|session| session.id == session_id)
    }

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
