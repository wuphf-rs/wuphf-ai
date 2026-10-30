//! Chat events: what WhatsApp reports about chats rather than messages (group
//! notifications, history syncs), mapped to the core's chat updates.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;
use whatsapp_rust::client::Client;
use whatsapp_rust::types::events::Event;
use whatsapp_rust::{Jid, Server};
use wuphf_proto::connector as pb;

use crate::disappearing::{CHAT_UPDATE_BATCH, chat_update_frames, group_update, history_updates};
use crate::groups::GroupDirectory;
use crate::normalize::canonical;

/// Handles group notifications and history syncs. Cheap to clone: every
/// handle shares the one frame sink and group directory.
#[derive(Clone)]
pub struct ChatEvents {
    inputs: mpsc::Sender<pb::ClientFrame>,
    groups: Arc<GroupDirectory>,
}

impl ChatEvents {
    pub fn new(inputs: mpsc::Sender<pb::ClientFrame>, groups: Arc<GroupDirectory>) -> Self {
        Self { inputs, groups }
    }

    pub async fn handle(&self, event: Arc<Event>) {
        let updates = match &*event {
            Event::GroupUpdate(update) => {
                // The subject or the timer may have changed.
                self.groups.forget(&update.group_jid);
                group_update(update).into_iter().collect()
            }
            Event::HistorySync(_) => {
                // Decoding a history sync is CPU-heavy; keep it off the runtime.
                let event = Arc::clone(&event);
                let decoded = tokio::task::spawn_blocking(move || match &*event {
                    Event::HistorySync(sync) => sync.get().map(history_updates),
                    _ => None,
                })
                .await;
                match decoded {
                    Ok(Some(updates)) => updates,
                    Ok(None) => {
                        warn!(
                            "could not decode a WhatsApp history sync; its chat timers are skipped"
                        );
                        return;
                    }
                    Err(error) => {
                        warn!(%error, "decoding a WhatsApp history sync failed");
                        return;
                    }
                }
            }
            _ => return,
        };
        for frame in chat_update_frames(updates, CHAT_UPDATE_BATCH) {
            if self.inputs.send(frame).await.is_err() {
                warn!("core session input closed; dropping WhatsApp chat updates");
                return;
            }
        }
    }
}

/// Every form the core may know a chat by: its canonical id, plus for a DM the
/// other (phone or LID) form when the client knows the mapping. This
/// connector reports no identity migrations, so the core cannot link the two
/// forms itself.
pub async fn chat_forms(client: &Client, jid: &Jid) -> Vec<String> {
    let jid = jid.to_non_ad();
    let mut forms = vec![canonical(&jid)];
    if !matches!(jid.server, Server::Pn | Server::Lid) {
        return forms;
    }
    if let Ok(Some(entry)) = client.get_lid_pn_entry(&jid).await {
        let other = if jid.is_lid() {
            Jid::pn(&*entry.phone_number)
        } else {
            Jid::lid(&*entry.lid)
        };
        let other = canonical(&other);
        if !forms.contains(&other) {
            forms.push(other);
        }
    }
    forms
}
