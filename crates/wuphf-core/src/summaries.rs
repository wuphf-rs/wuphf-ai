//! Reaction counts on posts (plan 085): a chat whose connector reports
//! `summaries` (a WhatsApp channel) is polled every
//! `channels.counts_poll_minutes` for the posts whose counts changed; the
//! connector answers with one [`EventKind::ReactionSummary`] per post. The
//! core keeps the counts per message and, when they change, edits the post's
//! mirror copies with a footer line (`❤️ 120 · 👍 34`) under its text. A later
//! edit of the post keeps the footer.
//!
//! Only copies the bridge wrote get the footer: it is the bridge's to write,
//! and the copy the counts were read on is the platform's own. A post older
//! than `channels.counts_max_age_days` is no longer updated.
//!
//! The poll schedule is in memory, filled from the posting facts each
//! connector reports when it connects: a restart polls each chat from
//! `counts_max_age_days` ago once, and only counts that differ from the stored
//! ones cause edits.
//!
//! [`EventKind::ReactionSummary`]: wuphf_model::EventKind::ReactionSummary

use std::collections::HashMap;
use std::sync::Arc;

use wuphf_model::{
    Action, ChatAddress, ConnectorId, Dispatch, EndpointId, IgnoreReason, IngestOutcome, Message,
    ReactionCount,
};
use wuphf_storage::{Storage, StoredSummary};

use crate::{Core, CoreError, RoutingConfig};

/// Separates the post's text from its footer.
const FOOTER_GAP: &str = "\n\n";

/// Separates two counts in a footer.
const COUNT_SEPARATOR: &str = " · ";

/// A chat due a poll: ask its connector for the counts that changed since
/// `since_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionPoll {
    pub connector_id: ConnectorId,
    pub chat_id: String,
    pub since_ms: i64,
}

/// The chats whose connector reports summaries, with when each was last
/// polled (`None`: not since startup).
pub(crate) type PollSchedule = HashMap<(ConnectorId, String), Option<i64>>;

impl<S> Core<S>
where
    S: Storage,
{
    /// Note whether a chat's connector reports summaries for it, from the
    /// posting it reported.
    pub(crate) fn schedule_summaries(
        &self,
        connector_id: &ConnectorId,
        chat_id: &str,
        reports: bool,
    ) {
        let key = (connector_id.clone(), chat_id.to_owned());
        let mut schedule = self.reaction_polls.lock().unwrap();
        if reports {
            schedule.entry(key).or_insert(None);
        } else {
            schedule.remove(&key);
        }
    }

    /// The mirrored chats due a poll at `now_ms`, each with the time since
    /// which changed counts are wanted: its last poll, or the oldest post that
    /// still updates. None while `channels.counts_poll_minutes` is 0. A poll is
    /// due again only once [`Core::mark_reaction_polled`] says it was sent.
    pub async fn due_reaction_polls(&self, now_ms: i64) -> Result<Vec<ReactionPoll>, CoreError> {
        let routing = self.routing.routing();
        let channels = routing.channels();
        let Some(every) = channels.poll_every_ms() else {
            return Ok(Vec::new());
        };
        let oldest = now_ms - channels.max_age_ms();
        let due: Vec<_> = self
            .reaction_polls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, last)| last.is_none_or(|at| now_ms - at >= every))
            .map(|((connector_id, chat_id), last)| ReactionPoll {
                connector_id: connector_id.clone(),
                chat_id: chat_id.clone(),
                since_ms: last.unwrap_or(oldest).max(oldest),
            })
            .collect();
        let mut polls = Vec::new();
        for poll in due {
            let address = ChatAddress::new(poll.chat_id.clone(), None);
            if self
                .storage
                .resolve_endpoint(&poll.connector_id, &address)
                .await?
                .is_some()
            {
                polls.push(poll);
            }
        }
        polls.sort_by(|a, b| (&a.connector_id.0, &a.chat_id).cmp(&(&b.connector_id.0, &b.chat_id)));
        Ok(polls)
    }

    /// Record that `poll` was sent at `now_ms`.
    pub fn mark_reaction_polled(&self, poll: &ReactionPoll, now_ms: i64) {
        let key = (poll.connector_id.clone(), poll.chat_id.clone());
        if let Some(last) = self.reaction_polls.lock().unwrap().get_mut(&key) {
            *last = Some(now_ms);
        }
    }

    /// A post's counts as its connector read them: stored, and the footer of
    /// its mirror copies edited when they differ from the stored ones.
    pub(crate) async fn ingest_reaction_summary(
        &self,
        routing: &Arc<RoutingConfig>,
        origin_endpoint: EndpointId,
        native_id: String,
        counts: Vec<ReactionCount>,
        received_at_ms: i64,
    ) -> Result<IngestOutcome, CoreError> {
        let Some((message_id, message)) = self
            .message_for_copy(origin_endpoint, &native_id, CoreError::EditTargetNotStored)
            .await?
        else {
            return Ok(IngestOutcome::Ignored(IgnoreReason::Unmatched));
        };
        let counts = in_footer_order(counts);
        let stored = self.storage.reaction_summary(message_id).await?;
        let unchanged = match &stored {
            Some(stored) => stored.counts == counts,
            // Nothing to show, and no footer to take away.
            None => counts.is_empty(),
        };
        let too_old = message.timestamp_ms < received_at_ms - routing.channels().max_age_ms();
        if message.deleted || unchanged || too_old {
            return Ok(IngestOutcome::Ignored(IgnoreReason::UnchangedSummary));
        }
        let summary = StoredSummary {
            endpoint: origin_endpoint,
            counts,
        };
        self.storage
            .put_reaction_summary(message_id, &summary)
            .await?;

        let routed = self
            .with_routing(routing)
            .route_edit(&message, origin_endpoint)
            .await?;
        let mut edits = Vec::new();
        for dispatch in routed {
            if self.bridge_wrote(&dispatch).await? {
                edits.push(dispatch);
            }
        }
        with_footers(&mut edits, &summary);
        let (dispatches, blocked) = self.save_dispatches(routing, edits).await?;
        Ok(IngestOutcome::Routed {
            message_id,
            dispatches,
            blocked,
        })
    }

    /// Put `message`'s footer, if it has counts, on its edits bound for
    /// copies other than the one they were read on: an edit of a post keeps
    /// its footer.
    pub(crate) async fn keep_footers(
        &self,
        message: &Message,
        dispatches: &mut [Dispatch],
    ) -> Result<(), CoreError> {
        if let Some(summary) = self.storage.reaction_summary(message.id).await? {
            with_footers(dispatches, &summary);
        }
        Ok(())
    }

    /// Whether an edit goes to a copy the bridge wrote (or will write: one not
    /// delivered yet).
    async fn bridge_wrote(&self, dispatch: &Dispatch) -> Result<bool, CoreError> {
        let Action::Edit { native_id, .. } = &dispatch.action else {
            return Ok(false);
        };
        Ok(native_id.is_empty()
            || self
                .storage
                .is_sent_copy(dispatch.to.endpoint_id, native_id)
                .await?)
    }
}

/// The footer on every edit not bound for the copy the counts were read on.
fn with_footers(dispatches: &mut [Dispatch], summary: &StoredSummary) {
    let footer = footer(&summary.counts);
    for dispatch in dispatches {
        if dispatch.to.endpoint_id == summary.endpoint {
            continue;
        }
        if let Action::Edit { message, .. } = &mut dispatch.action {
            message.text = with_footer(message.text.as_deref(), footer.as_deref());
        }
    }
}

/// Counts as a footer shows them: none at zero, most first, ties by emoji.
fn in_footer_order(counts: Vec<ReactionCount>) -> Vec<ReactionCount> {
    let mut counts: Vec<_> = counts
        .into_iter()
        .filter(|count| count.count > 0 && !count.emoji.is_empty())
        .collect();
    counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.emoji.cmp(&b.emoji)));
    counts
}

/// The footer line of `counts` (in footer order): `❤️ 120 · 👍 34`; none
/// without reactions.
pub(crate) fn footer(counts: &[ReactionCount]) -> Option<String> {
    (!counts.is_empty()).then(|| {
        counts
            .iter()
            .map(|count| format!("{} {}", count.emoji, count.count))
            .collect::<Vec<_>>()
            .join(COUNT_SEPARATOR)
    })
}

/// A post's text with its footer under it; the footer alone for a post
/// without text (a photo), the text alone without a footer.
pub(crate) fn with_footer(text: Option<&str>, footer: Option<&str>) -> Option<String> {
    match (text.filter(|text| !text.is_empty()), footer) {
        (Some(text), Some(footer)) => Some(format!("{text}{FOOTER_GAP}{footer}")),
        (None, Some(footer)) => Some(footer.to_owned()),
        (text, None) => text.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(emoji: &str, count: u32) -> ReactionCount {
        ReactionCount {
            emoji: emoji.into(),
            count,
        }
    }

    #[test]
    fn a_footer_lists_the_most_used_reactions_first() {
        let counts = in_footer_order(vec![
            count("👍", 34),
            count("😂", 0),
            count("❤️", 120),
            count("🔥", 34),
        ]);
        assert_eq!(footer(&counts).as_deref(), Some("❤️ 120 · 👍 34 · 🔥 34"));
        assert_eq!(footer(&[]), None);
    }

    #[test]
    fn the_footer_goes_under_the_text() {
        assert_eq!(
            with_footer(Some("News"), Some("❤️ 1")).as_deref(),
            Some("News\n\n❤️ 1")
        );
        assert_eq!(with_footer(None, Some("❤️ 1")).as_deref(), Some("❤️ 1"));
        assert_eq!(with_footer(Some(""), Some("❤️ 1")).as_deref(), Some("❤️ 1"));
        assert_eq!(with_footer(Some("News"), None).as_deref(), Some("News"));
        assert_eq!(with_footer(None, None), None);
    }
}
