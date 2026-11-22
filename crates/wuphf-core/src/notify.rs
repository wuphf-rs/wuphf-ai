//! Owner notices: a connector telling the account owner that it is in trouble,
//! delivered through the connectors that still work.
//!
//! A connector that loses its session goes quiet, and a bridge that has quietly
//! stopped bridging is worse than one that says so. The connector reports only
//! what happened ([`NoticeKind`]); everything else is decided here — the
//! wording, which chats it reaches, and whether it is worth sending at all.
//!
//! Delivery reuses the raw-send path, so a notice is an ordinary durable
//! dispatch: it survives a restart, redelivers if the target connector is
//! itself offline, and shows up in the observation log like any other message.

use crate::{Core, CoreError};
use wuphf_model::{ConnectorId, Dispatch, MediaId, NoticeKind, OwnerMessage};
use wuphf_storage::Storage;

impl<S> Core<S>
where
    S: Storage,
{
    /// Relay a connector's notice to the owner, returning the dispatches to
    /// deliver. Empty when no target is configured, when every target is
    /// served by the reporting connector, or when a health notice repeats the
    /// state already sent. `media_ids` go with every message of the notice;
    /// one the store does not hold is left out rather than failing the notice.
    pub async fn notify_owners(
        &self,
        connector_id: ConnectorId,
        kind: NoticeKind,
        detail: &str,
        media_ids: &[MediaId],
        timestamp_ms: i64,
    ) -> Result<Vec<Dispatch>, CoreError> {
        // Only health kinds go through the per-connector state: a notice about
        // one chat (an unknown timer) neither repeats a health state nor
        // resets one, and the connector limits its own repeats.
        if kind.is_health() && !self.notice_is_new(&connector_id, kind) {
            return Ok(Vec::new());
        }
        let text = notice_text(&connector_id, kind, detail);
        let mut stored = Vec::with_capacity(media_ids.len());
        for &media_id in media_ids {
            if self.storage.media(media_id).await?.is_some() {
                stored.push(media_id);
            }
        }
        // One snapshot for the whole notice: the targets and every send to them.
        let routing = self.routing.routing();
        let targets: Vec<_> = routing
            .notice_targets_excluding(&connector_id)
            .cloned()
            .collect();

        let mut dispatches = Vec::new();
        for target in targets {
            let outcome = self
                .send_owner_message_with(
                    &routing,
                    OwnerMessage {
                        connector_id: target.connector_id,
                        chat_id: target.address.chat_id,
                        thread_id: target.address.thread_id,
                        text: text.clone(),
                        media_ids: stored.clone(),
                        client: None,
                        reply_to_native_id: None,
                        timestamp_ms,
                    },
                )
                .await?;
            dispatches.push(outcome.direct);
            dispatches.extend(outcome.mirrored);
        }
        Ok(dispatches)
    }

    /// Whether this notice says something new about the connector, recording it
    /// as the connector's current state when it does.
    ///
    /// A connector in trouble tends to keep saying so — a reconnect loop can
    /// report the same logout every few seconds — and the owner needs to be told
    /// once, not paged continuously. Suppression is by state rather than by
    /// elapsed time, which keeps the core clock-free: the same kind twice in a
    /// row is silence, and any change (including a recovery) speaks again. The
    /// state is in memory, so a restart deliberately re-notifies: that is the
    /// point at which the owner most wants to know the connector is still down.
    fn notice_is_new(&self, connector_id: &ConnectorId, kind: NoticeKind) -> bool {
        let mut notices = self.notices.lock().unwrap();
        let previous = notices.get(connector_id).copied();
        if previous == Some(kind) {
            return false;
        }
        // A recovery is only news after trouble. Connectors report health on
        // every (re)connect, so without this every start-up and every dropped
        // network link would announce that nothing is wrong.
        if kind == NoticeKind::Recovered && previous.is_none() {
            return false;
        }
        notices.insert(connector_id.clone(), kind);
        true
    }
}

/// Compose the message the owner reads. The connector is named because a notice
/// arrives somewhere unrelated to it — a WhatsApp logout shows up on Telegram,
/// where nothing else says which bridge went down.
fn notice_text(connector_id: &ConnectorId, kind: NoticeKind, detail: &str) -> String {
    let marker = match kind {
        NoticeKind::Recovered => "✅",
        NoticeKind::LinkRequired => "🔗",
        _ => "⚠️",
    };
    let mut text = format!("{marker} {} {}", connector_id.0, kind.summary());
    let detail = detail.trim();
    if !detail.is_empty() {
        text.push_str(" — ");
        text.push_str(detail);
    }
    text
}
