//! The observation tap: mirrors every inbound connector event into the
//! observation log and a live broadcast, independent of routing. Read-only
//! consumers (an observer service, a TUI) query the log for history and
//! subscribe to the broadcast for new activity. The tap never consults routing,
//! so it captures every chat a connector reports, not only the bridged ones.

use std::sync::Arc;
use tokio::sync::broadcast;
use wuphf_model::{EventPayload, NormalizedEvent, ObservedDirection, ObservedMessage, Platform};
use wuphf_storage::{Storage, StorageError};

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
}

impl<S> Observer<S>
where
    S: Storage,
{
    pub fn new(storage: Arc<S>) -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self { storage, events }
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
    pub async fn observe(
        &self,
        event: &NormalizedEvent,
        platform: Platform,
    ) -> Result<(), StorageError> {
        let connector_id = &event.source.connector_id;
        let chat_id = &event.source.address.chat_id;
        let captured = match &event.payload {
            EventPayload::MessageCreate(inbound) => {
                let observed = ObservedMessage {
                    connector_id: connector_id.clone(),
                    platform,
                    chat_id: chat_id.clone(),
                    thread_id: event.source.address.thread_id.clone(),
                    platform_message_id: inbound.platform_message_id.clone(),
                    sender: inbound.sender.clone(),
                    sender_name: inbound.sender_name.clone(),
                    participant: inbound.participant.clone(),
                    text: inbound.text.clone(),
                    attachments: inbound.attachments.clone(),
                    reply_to_platform_message_id: inbound.reply_to_platform_message_id.clone(),
                    direction: ObservedDirection::Incoming,
                    timestamp_ms: inbound.timestamp_ms,
                    deleted: false,
                    edited: false,
                };
                self.storage
                    .log_observed(observed.clone(), event.source.chat_title.clone())
                    .await?;
                Some(observed)
            }
            EventPayload::MessageEdit {
                platform_message_id,
                text,
            } => {
                self.storage
                    .mark_observed_edited(connector_id, chat_id, platform_message_id, text.clone())
                    .await?
            }
            EventPayload::MessageDelete {
                platform_message_id,
            } => {
                self.storage
                    .mark_observed_deleted(connector_id, chat_id, platform_message_id)
                    .await?
            }
            EventPayload::ReactionSet { .. } | EventPayload::ReceiptSet { .. } => None,
        };
        if let Some(observed) = captured {
            // A send error means no live subscribers; history is already
            // persisted, so dropping the live copy is fine.
            let _ = self.events.send(observed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{ConnectorId, InboundMessage, PlatformAddress, SourceAddress};
    use wuphf_storage::InMemoryStorage;

    fn create_event(chat: &str, id: &str, text: &str, title: Option<&str>) -> NormalizedEvent {
        NormalizedEvent {
            source: SourceAddress {
                connector_id: ConnectorId("whatsapp".into()),
                address: PlatformAddress::new(chat, None),
                chat_title: title.map(Into::into),
            },
            payload: EventPayload::MessageCreate(InboundMessage {
                platform_message_id: id.into(),
                sender: "peer".into(),
                participant: None,
                sender_name: Some("Peer".into()),
                text: Some(text.into()),
                attachments: vec![],
                reply_to_platform_message_id: None,
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
        assert_eq!(broadcast.platform_message_id, "a1");

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

        let edit = NormalizedEvent {
            source: SourceAddress {
                connector_id: connector.clone(),
                address: PlatformAddress::new("alice", None),
                chat_title: None,
            },
            payload: EventPayload::MessageEdit {
                platform_message_id: "a1".into(),
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

        let delete = NormalizedEvent {
            source: SourceAddress {
                connector_id: connector.clone(),
                address: PlatformAddress::new("alice", None),
                chat_title: None,
            },
            payload: EventPayload::MessageDelete {
                platform_message_id: "a1".into(),
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
    async fn observe_ignores_events_without_a_known_message() {
        let observer = Observer::new(Arc::new(InMemoryStorage::default()));
        // An edit for a message that was never observed is a no-op, not an error.
        let edit = NormalizedEvent {
            source: SourceAddress {
                connector_id: ConnectorId("whatsapp".into()),
                address: PlatformAddress::new("alice", None),
                chat_title: None,
            },
            payload: EventPayload::MessageEdit {
                platform_message_id: "ghost".into(),
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
