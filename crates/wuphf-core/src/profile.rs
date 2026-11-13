//! Chat profiles: what the core knows about each platform chat apart from
//! routing. Connectors report what they observe; the core merges reports and
//! hands the result to sends.
//!
//! A profile is keyed by the connector and the chat's native id, never by
//! endpoint, and is recorded before routing: a chat no rule matches still
//! learns its timer, so a later send into it (an owner reply, a new rule) can
//! carry it.

use crate::config::RoutingConfig;
use crate::resolve::address_forms;
use crate::{Core, CoreError};
use std::collections::HashSet;
use wuphf_model::{
    ChatAddress, ChatProfile, ChatUpdate, ConnectorId, DisappearingTimer, SourceAddress, Target,
};
use wuphf_storage::{ChatProfileStore, Storage, StorageError, TopologyStore};

/// Merge a reported timer into the stored one, returning the new value when it
/// changes. With a setting timestamp on both sides the newer wins (an equal
/// one counts as newer); otherwise the later report wins. A report that
/// repeats the stored seconds without a timestamp keeps the stored timestamp.
pub(crate) fn merge_disappearing(
    stored: Option<DisappearingTimer>,
    incoming: DisappearingTimer,
) -> Option<DisappearingTimer> {
    let next = match stored {
        None => incoming,
        Some(current) => match (current.setting_timestamp, incoming.setting_timestamp) {
            (Some(known), Some(reported)) if reported < known => return None,
            _ if incoming.seconds == current.seconds => DisappearingTimer {
                seconds: current.seconds,
                setting_timestamp: incoming.setting_timestamp.or(current.setting_timestamp),
            },
            _ => incoming,
        },
    };
    (stored != Some(next)).then_some(next)
}

/// Apply one report to a profile; true when anything changed.
pub(crate) fn apply(profile: &mut ChatProfile, update: &ChatUpdate) -> bool {
    let mut changed = false;
    if let Some(reported) = update.disappearing
        && let Some(next) = merge_disappearing(profile.disappearing, reported)
    {
        profile.disappearing = Some(next);
        changed = true;
    }
    changed
}

impl<S> Core<S>
where
    S: Storage,
{
    /// Record chat properties a connector reported, returning how many chats
    /// changed. Runs on the connector's serial ingest worker, so a
    /// read-merge-write cannot race another update for the same connector.
    pub async fn update_chats(
        &self,
        connector_id: &ConnectorId,
        updates: Vec<ChatUpdate>,
    ) -> Result<usize, CoreError> {
        let mut changed = 0;
        for update in &updates {
            if self.update_chat(connector_id, update).await? {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Merge one report into the stored profile, writing only on a change.
    async fn update_chat(
        &self,
        connector_id: &ConnectorId,
        update: &ChatUpdate,
    ) -> Result<bool, CoreError> {
        let mut profile = self
            .storage
            .chat_profile(connector_id, &update.chat_id)
            .await?
            .unwrap_or_else(|| ChatProfile::new(connector_id.clone(), update.chat_id.clone()));
        if !apply(&mut profile, update) {
            return Ok(false);
        }
        self.storage.put_chat_profile(&profile).await?;
        Ok(true)
    }

    /// Record what an inbound message says about its chat's timer. Runs before
    /// routing, so chats no rule matches are recorded too.
    ///
    /// A message that carries a timer is a report like any other. A message
    /// without one, from a connector that stamps timers, settles a chat whose
    /// timer is still unknown as off: the platform would have put the timer on
    /// the message had the chat one. That fill has no setting timestamp, so
    /// any later report replaces it, and it never touches a known timer (the
    /// merge rule would let a timestamp-less 0 replace one, so it is written
    /// directly rather than merged).
    pub(crate) async fn observe_message_timer(
        &self,
        routing: &RoutingConfig,
        source: &SourceAddress,
        timer: Option<DisappearingTimer>,
    ) -> Result<(), CoreError> {
        let chat_id = &source.address.chat_id;
        if let Some(timer) = timer {
            self.update_chat(
                &source.connector_id,
                &ChatUpdate {
                    chat_id: chat_id.clone(),
                    disappearing: Some(timer),
                    ..ChatUpdate::default()
                },
            )
            .await?;
            return Ok(());
        }
        if !stamps_timers(routing, &source.connector_id) {
            return Ok(());
        }
        let stored = self
            .storage
            .chat_profile(&source.connector_id, chat_id)
            .await?;
        if stored
            .as_ref()
            .is_some_and(|profile| profile.disappearing.is_some())
        {
            return Ok(());
        }
        let mut profile = stored
            .unwrap_or_else(|| ChatProfile::new(source.connector_id.clone(), chat_id.clone()));
        profile.disappearing = Some(DisappearingTimer::new(0));
        self.storage.put_chat_profile(&profile).await?;
        Ok(())
    }
}

/// Whether the connector declares that it stamps disappearing timers.
fn stamps_timers(routing: &RoutingConfig, connector_id: &ConnectorId) -> bool {
    routing
        .connector(connector_id)
        .is_some_and(|connector| connector.capabilities.supports_disappearing_timers)
}

/// The timer a send to `target` carries: `None` when the target's connector
/// does not declare supports_disappearing_timers, or when no profile of the
/// chat (under its own id, then under any linked form, newest timestamp
/// first) has one.
pub(crate) async fn target_disappearing<S: TopologyStore + ChatProfileStore>(
    storage: &S,
    routing: &RoutingConfig,
    target: &Target,
) -> Result<Option<DisappearingTimer>, StorageError> {
    if !stamps_timers(routing, &target.connector_id) {
        return Ok(None);
    }
    let own = ChatAddress::new(target.address.chat_id.clone(), None);
    let forms = address_forms(storage, &target.connector_id, &own).await?;
    let mut seen = HashSet::new();
    let mut best: Option<DisappearingTimer> = None;
    // The set starts with the target's own address, so its profile is read
    // first and a linked form only wins with a strictly newer timestamp.
    for form in forms.iter() {
        if !seen.insert(form.chat_id.as_str()) {
            continue;
        }
        let Some(timer) = storage
            .chat_profile(&target.connector_id, &form.chat_id)
            .await?
            .and_then(|profile| profile.disappearing)
        else {
            continue;
        };
        if best.is_none_or(|current| timer.setting_timestamp > current.setting_timestamp) {
            best = Some(timer);
        }
    }
    Ok(best)
}
