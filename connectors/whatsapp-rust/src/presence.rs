//! Keeps this session offline. whatsapp-rust marks the client available
//! ("online") after every connect and whenever the push name syncs, as
//! WhatsApp Web does when its tab is open. While a linked device is online,
//! WhatsApp holds back the phone's notifications, so with the bridge always
//! connected they arrive late or not at all. The Go connector never sends
//! presence; this undoes the library's.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};
use whatsapp_rust::PresenceError;
use whatsapp_rust::client::Client;

/// How long after the event the library's own "available" has gone out. It
/// sends it just before Connected on a reconnect, and just after
/// SelfPushNameUpdated, from a task of its own.
const AFTER_LIBRARY: Duration = Duration::from_secs(3);

/// Marks the session unavailable once the library's "available" has gone
/// out. Off the event path.
pub fn stay_offline(client: Arc<Client>) {
    tokio::spawn(async move {
        tokio::time::sleep(AFTER_LIBRARY).await;
        match client.presence().set_unavailable().await {
            Ok(()) => debug!("marked the WhatsApp session unavailable"),
            // Before the first app-state sync there is no push name, and the
            // library sends no presence either; SelfPushNameUpdated follows.
            Err(PresenceError::PushNameEmpty) => {}
            Err(error) => warn!(?error, "could not mark the WhatsApp session unavailable"),
        }
    });
}
