//! The observation tap: mirrors every inbound connector event into the
//! observation log and a live broadcast, independent of routing. Read-only
//! consumers (an observer service, a TUI) query the log for history and
//! subscribe to the broadcast for new activity. The tap never consults routing,
//! so it captures every chat a connector reports, not only the bridged ones.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, oneshot};
use wuphf_model::{DispatchId, Event, EventKind, ObservedMessage, Platform};
use wuphf_storage::{ObservationStore, StorageError};

/// How many recent observed messages the live broadcast retains for a lagging
/// subscriber before dropping the oldest. Bridging is not high-frequency, so a
/// modest buffer covers a subscriber that stalls briefly; a subscriber that
/// falls further behind sees a lag error and can re-query history.
const BROADCAST_CAPACITY: usize = 1024;

/// Captures inbound events into the observation log and republishes them live.
/// Cheap to clone-share via [`Arc`]; a single instance backs the ingest tap and
/// every read-only consumer.
pub struct Observer<S> {
    storage: Arc<S>,
    events: broadcast::Sender<ObservedMessage>,
    /// Owner messages in flight, keyed by their direct dispatch. The observed row is
    /// held here until the connector's acknowledgement supplies the native
    /// message id, at which point it is logged as outgoing and broadcast. The
    /// lock is never held across an await.
    pending_outbound: Mutex<HashMap<DispatchId, PendingOutbound>>,
}

/// One owner message awaiting its acknowledgement: the observed row missing only its
/// native id, and the waiter (if any) to notify with the settled row.
struct PendingOutbound {
    observed: ObservedMessage,
    notify: Option<oneshot::Sender<ObservedMessage>>,
}

impl<S> Observer<S>
where
    S: ObservationStore,
{
    pub fn new(storage: Arc<S>) -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            storage,
            events,
            pending_outbound: Mutex::new(HashMap::new()),
        }
    }

    /// Storage handle for read-only consumers to query observation history.
    pub fn storage(&self) -> Arc<S> {
        Arc::clone(&self.storage)
    }

    /// Subscribe to observed messages as they are captured. Each captured event
    /// (a new message, or an edit/delete carrying the updated row) is delivered;
    /// a consumer upserts by native coordinates, honoring the `edited`/`deleted`
    /// flags.
    pub fn subscribe(&self) -> broadcast::Receiver<ObservedMessage> {
        self.events.subscribe()
    }

    /// Capture one inbound event. A create is logged as a new message; an edit or
    /// delete updates the existing row (and is skipped when the referenced
    /// message was never observed). Reactions and receipts change no message
    /// content and are not logged. `platform` labels the message for display.
    ///
    /// Whatever row results is broadcast to live subscribers. Observation is a
    /// side concern: a failure is returned for the caller to log, never to abort
    /// ingest.
    pub async fn observe(&self, event: &Event, platform: Platform) -> Result<(), StorageError> {
        let connector_id = &event.source.connector_id;
        let chat_id = &event.source.address.chat_id;
        let captured = match &event.payload {
            EventKind::NewMessage(inbound) => {
                let observed = ObservedMessage::incoming(&event.source, platform, inbound);
                // Broadcast the row as stored, not as constructed: an echo of
                // our own owner message keeps its outgoing direction in storage, and
                // live subscribers must see the same row.
                Some(
                    self.storage
                        .log_observed(observed, event.source.chat_title.clone())
                        .await?,
                )
            }
            EventKind::Edit {
                native_id: platform_message_id,
                text,
            } => {
                self.storage
                    .mark_observed_edited(connector_id, chat_id, platform_message_id, text.clone())
                    .await?
            }
            EventKind::Delete {
                native_id: platform_message_id,
            } => {
                self.storage
                    .mark_observed_deleted(connector_id, chat_id, platform_message_id)
                    .await?
            }
            EventKind::Reaction { .. } | EventKind::Receipt { .. } => None,
        };
        if let Some(observed) = captured {
            // A send error means no live subscribers; history is already
            // persisted, so dropping the live copy is fine.
            let _ = self.events.send(observed);
        }
        Ok(())
    }

    /// Register an owner message awaiting acknowledgement. The observed row (its
    /// `platform_message_id` still unknown) is held until
    /// [`settle_outbound`](Self::settle_outbound) fills it in from the
    /// connector's acknowledgement. The returned receiver resolves with the
    /// settled row; dropping it is fine — the row is logged either way.
    pub fn track_outbound(
        &self,
        dispatch_id: DispatchId,
        observed: ObservedMessage,
    ) -> oneshot::Receiver<ObservedMessage> {
        let (notify, receiver) = oneshot::channel();
        self.pending_outbound.lock().unwrap().insert(
            dispatch_id,
            PendingOutbound {
                observed,
                notify: Some(notify),
            },
        );
        receiver
    }

    /// Settle a tracked owner message with the native id its acknowledgement
    /// carried: log the row as outgoing, broadcast it live, and notify the
    /// waiter. A dispatch that was never tracked (every ordinary bridge
    /// dispatch) is a no-op.
    pub async fn settle_outbound(
        &self,
        dispatch_id: DispatchId,
        platform_message_id: &str,
    ) -> Result<(), StorageError> {
        let Some(mut pending) = self.pending_outbound.lock().unwrap().remove(&dispatch_id) else {
            return Ok(());
        };
        pending.observed.native_id = platform_message_id.to_owned();
        let stored = self.storage.log_observed(pending.observed, None).await?;
        if let Some(notify) = pending.notify.take() {
            let _ = notify.send(stored.clone());
        }
        let _ = self.events.send(stored);
        Ok(())
    }

    /// Drop a tracked owner message whose dispatch failed permanently; nothing was
    /// delivered, so nothing is logged. Untracked dispatches are a no-op.
    pub fn discard_outbound(&self, dispatch_id: DispatchId) {
        self.pending_outbound.lock().unwrap().remove(&dispatch_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{ChatAddress, ConnectorId, NewMessage, ObservedDirection, SourceAddress};
    use wuphf_storage::{InMemoryStorage, ObservationStore};

    fn create_event(chat: &str, id: &str, text: &str, title: Option<&str>) -> Event {
        Event {
            source: SourceAddress {
                connector_id: ConnectorId("whatsapp".into()),
                address: ChatAddress::new(chat, None),
                chat_title: title.map(Into::into),
            },
            payload: EventKind::NewMessage(NewMessage {
                native_id: id.into(),
                author_id: "peer".into(),
                participant: None,
                author_name: Some("Peer".into()),
                text: Some(text.into()),
                attachments: vec![],
                reply_to_native_id: None,
                timestamp_ms: 100,
            }),
        }
    }

    #[tokio::test]
    async fn observe_persists_and_broadcasts_a_create() {
        let observer = Observer::new(Arc::new(InMemoryStorage::default()));
        let mut live = observer.subscribe();
        observer
            .observe(
                &create_event("alice", "a1", "hi", Some("Alice")),
                Platform::WhatsApp,
            )
            .await
            .unwrap();

        let broadcast = live.try_recv().expect("live message delivered");
        assert_eq!(broadcast.native_id, "a1");

        let chats = observer.storage().list_observed_chats().await.unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].title.as_deref(), Some("Alice"));
        let page = observer
            .storage()
            .observed_messages(&ConnectorId("whatsapp".into()), "alice", None, 10)
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].text.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn observe_updates_on_edit_and_delete() {
        let observer = Observer::new(Arc::new(InMemoryStorage::default()));
        let connector = ConnectorId("whatsapp".into());
        observer
            .observe(&create_event("alice", "a1", "hi", None), Platform::WhatsApp)
            .await
            .unwrap();

        let edit = Event {
            source: SourceAddress {
                connector_id: connector.clone(),
                address: ChatAddress::new("alice", None),
                chat_title: None,
            },
            payload: EventKind::Edit {
                native_id: "a1".into(),
                text: Some("hello".into()),
            },
        };
        observer.observe(&edit, Platform::WhatsApp).await.unwrap();
        let page = observer
            .storage()
            .observed_messages(&connector, "alice", None, 10)
            .await
            .unwrap();
        assert!(page[0].edited && page[0].text.as_deref() == Some("hello"));

        let delete = Event {
            source: SourceAddress {
                connector_id: connector.clone(),
                address: ChatAddress::new("alice", None),
                chat_title: None,
            },
            payload: EventKind::Delete {
                native_id: "a1".into(),
            },
        };
        observer.observe(&delete, Platform::WhatsApp).await.unwrap();
        let page = observer
            .storage()
            .observed_messages(&connector, "alice", None, 10)
            .await
            .unwrap();
        assert!(page[0].deleted);
    }

    #[tokio::test]
    async fn settle_outbound_logs_and_notifies_only_tracked_dispatches() {
        let observer = Observer::new(Arc::new(InMemoryStorage::default()));
        let dispatch_id = wuphf_model::DispatchId::new();
        // An untracked dispatch (every ordinary bridge dispatch) is a no-op.
        observer.settle_outbound(dispatch_id, "tg-1").await.unwrap();
        assert!(
            observer
                .storage()
                .list_observed_chats()
                .await
                .unwrap()
                .is_empty()
        );

        let template = ObservedMessage {
            connector_id: ConnectorId("whatsapp".into()),
            platform: Platform::WhatsApp,
            chat_id: "alice".into(),
            thread_id: None,
            native_id: String::new(),
            author_id: "me".into(),
            author_name: None,
            participant: None,
            text: Some("mine".into()),
            attachments: vec![],
            reply_to_native_id: None,
            direction: ObservedDirection::Outgoing,
            timestamp_ms: 42,
            deleted: false,
            edited: false,
        };
        let mut live = observer.subscribe();
        let settled = observer.track_outbound(dispatch_id, template.clone());
        observer.settle_outbound(dispatch_id, "wa-1").await.unwrap();
        let row = settled.await.unwrap();
        assert_eq!(row.native_id, "wa-1");
        assert_eq!(row.direction, ObservedDirection::Outgoing);
        assert_eq!(live.try_recv().unwrap().native_id, "wa-1");
        let page = observer
            .storage()
            .observed_messages(&ConnectorId("whatsapp".into()), "alice", None, 10)
            .await
            .unwrap();
        assert_eq!(page.len(), 1);

        // A discarded (failed) send drops the waiter and logs nothing.
        let failed_id = wuphf_model::DispatchId::new();
        let waiter = observer.track_outbound(failed_id, template);
        observer.discard_outbound(failed_id);
        assert!(waiter.await.is_err());
    }

    #[tokio::test]
    async fn observe_ignores_events_without_a_known_message() {
        let observer = Observer::new(Arc::new(InMemoryStorage::default()));
        // An edit for a message that was never observed is a no-op, not an error.
        let edit = Event {
            source: SourceAddress {
                connector_id: ConnectorId("whatsapp".into()),
                address: ChatAddress::new("alice", None),
                chat_title: None,
            },
            payload: EventKind::Edit {
                native_id: "ghost".into(),
                text: Some("x".into()),
            },
        };
        observer.observe(&edit, Platform::WhatsApp).await.unwrap();
        assert!(
            observer
                .storage()
                .list_observed_chats()
                .await
                .unwrap()
                .is_empty()
        );
    }
}
