//! Delivery and read receipts, reported like the Go connector's
//! `HandleReceipt`: direct chats only (group receipts are per-participant
//! noise), delivered and read only. The core turns them into 👍 and 👀 on the
//! message's other copies and ignores the owner's own (`from_me`).

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;
use whatsapp_rust::Server;
use whatsapp_rust::types::events::Event;
use whatsapp_rust::types::message::MessageSource;
use whatsapp_rust::types::presence::ReceiptType;
use wuphf_proto::connector as pb;

use crate::normalize::{canonical, frame};

/// The receipt kinds the bridge surfaces. A `Sent` receipt (the library's
/// downgraded delivery for a peer that cannot receive), self-reads, plays and
/// retries carry no acknowledgement from the peer for the mirror.
pub fn receipt_kind(kind: &ReceiptType) -> Option<pb::ReceiptKind> {
    match kind {
        ReceiptType::Delivered => Some(pb::ReceiptKind::Delivered),
        ReceiptType::Read => Some(pb::ReceiptKind::Read),
        _ => None,
    }
}

/// One event per message id; none for a group.
pub fn receipt_events(
    source: &MessageSource,
    message_ids: &[String],
    kind: pb::ReceiptKind,
) -> Vec<pb::ReceiptEvent> {
    if source.is_group || source.chat.server == Server::Group {
        return Vec::new();
    }
    let chat = canonical(&source.chat);
    let observer = canonical(&source.sender);
    message_ids
        .iter()
        .map(|id| pb::ReceiptEvent {
            chat_id: chat.clone(),
            thread_id: None,
            platform_message_id: id.clone(),
            observer: observer.clone(),
            from_me: source.is_from_me,
            kind: kind.into(),
        })
        .collect()
}

/// Turns WhatsApp receipts into receipt events on the connector's input
/// channel.
#[derive(Clone)]
pub struct Receipts {
    inputs: mpsc::Sender<pb::ClientFrame>,
}

impl Receipts {
    pub fn new(inputs: mpsc::Sender<pb::ClientFrame>) -> Self {
        Self { inputs }
    }

    /// An event handler, so awaiting the input channel is safe here.
    pub async fn handle(&self, event: Arc<Event>) {
        let Event::Receipt(receipt) = &*event else {
            return;
        };
        let Some(kind) = receipt_kind(&receipt.r#type) else {
            return;
        };
        for event in receipt_events(&receipt.source, &receipt.message_ids, kind) {
            if self
                .inputs
                .send(frame(pb::client_frame::Body::Receipt(event)))
                .await
                .is_err()
            {
                warn!("core session input closed; dropping a WhatsApp receipt");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dm() -> MessageSource {
        MessageSource {
            chat: "1555@s.whatsapp.net".parse().unwrap(),
            sender: "1555@s.whatsapp.net".parse().unwrap(),
            ..Default::default()
        }
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn a_dm_receipt_reports_each_message() {
        let events = receipt_events(&dm(), &ids(&["wa-1", "wa-2"]), pb::ReceiptKind::Delivered);
        assert_eq!(events.len(), 2);
        for (event, id) in events.iter().zip(["wa-1", "wa-2"]) {
            assert_eq!(event.platform_message_id, id);
            assert_eq!(event.chat_id, "1555@s.whatsapp.net");
            assert_eq!(event.observer, "1555@s.whatsapp.net");
            assert_eq!(event.kind(), pb::ReceiptKind::Delivered);
            assert!(!event.from_me);
        }
    }

    #[test]
    fn a_read_receipt_is_read() {
        assert_eq!(
            receipt_kind(&ReceiptType::Read),
            Some(pb::ReceiptKind::Read)
        );
        assert_eq!(
            receipt_kind(&ReceiptType::Delivered),
            Some(pb::ReceiptKind::Delivered)
        );
    }

    #[test]
    fn groups_and_other_types_are_dropped() {
        let group = MessageSource {
            chat: "123@g.us".parse().unwrap(),
            is_group: true,
            ..dm()
        };
        assert!(receipt_events(&group, &ids(&["wa-1"]), pb::ReceiptKind::Read).is_empty());
        for kind in [
            ReceiptType::Sent,
            ReceiptType::ReadSelf,
            ReceiptType::Played,
            ReceiptType::Sender,
            ReceiptType::Retry,
        ] {
            assert_eq!(receipt_kind(&kind), None, "{kind:?}");
        }
    }

    #[test]
    fn devices_are_stripped_and_from_me_is_kept() {
        let source = MessageSource {
            sender: "1555:3@s.whatsapp.net".parse().unwrap(),
            is_from_me: true,
            ..dm()
        };
        let events = receipt_events(&source, &ids(&["wa-1"]), pb::ReceiptKind::Read);
        assert_eq!(events[0].observer, "1555@s.whatsapp.net");
        assert!(events[0].from_me);
    }
}
