//! Health pauses (plan 079): dispatches into a direction of a rule that
//! failed a health check are held instead of sent, and released in order when
//! the check passes.
//!
//! The runtime decides what is paused (`wuphf-health`) and sets it here
//! ([`Core::set_health_pauses`]); the core only matches dispatches against it.
//! A dispatch matches a pause by its conversation's rule, its direction (into
//! the conversation's source chat is `Back`, anywhere else `Forward`) and the
//! chat it goes to.
//!
//! A held dispatch that would create a mirror thread still counts as that
//! conversation's provisioning, so later messages wait in the provisioning
//! stash rather than each creating a thread when released.

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use wuphf_model::{ConversationId, Dispatch, FlowDirection, HealthPause};
use wuphf_storage::Storage;

use crate::{Core, CoreError};

/// What releasing a pause produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Released {
    /// The dispatches to send now, in the order they were held.
    pub dispatches: Vec<Dispatch>,
    /// Messages dropped because they were held since before the cutoff.
    pub dropped: usize,
}

impl<S> Core<S>
where
    S: Storage,
{
    /// The directions to hold from now on, replacing the previous set.
    /// Releasing what a cleared pause held is [`Self::release_held`].
    pub fn set_health_pauses(&self, pauses: BTreeSet<HealthPause>) {
        *self.health_pauses.write().unwrap() = Arc::new(pauses);
    }

    /// The directions held now.
    pub fn health_pauses(&self) -> Arc<BTreeSet<HealthPause>> {
        Arc::clone(&self.health_pauses.read().unwrap())
    }

    /// The pause `dispatch` falls under, if any. `conversations` caches the
    /// conversations looked up during one batch.
    pub(crate) async fn pause_for(
        &self,
        pauses: &BTreeSet<HealthPause>,
        dispatch: &Dispatch,
        conversations: &mut HashMap<ConversationId, Option<wuphf_model::Conversation>>,
    ) -> Result<Option<HealthPause>, CoreError> {
        if pauses.is_empty() {
            return Ok(None);
        }
        let conversation = match conversations.entry(dispatch.conversation_id) {
            Entry::Occupied(known) => known.into_mut(),
            Entry::Vacant(unknown) => {
                unknown.insert(self.storage.conversation(dispatch.conversation_id).await?)
            }
        };
        let Some(conversation) = conversation else {
            return Ok(None);
        };
        // A conversation outside every rule (an owner's direct message) is
        // never paused.
        let Some(rule) = &conversation.rule_id else {
            return Ok(None);
        };
        let into_source = conversation.source.connector_id == dispatch.to.connector_id
            && conversation.source.address.chat_id == dispatch.to.address.chat_id;
        let candidate = HealthPause {
            rule: rule.clone(),
            direction: if into_source {
                FlowDirection::Back
            } else {
                FlowDirection::Forward
            },
            connector: dispatch.to.connector_id.clone(),
            chat_id: dispatch.to.address.chat_id.clone(),
        };
        Ok(pauses.contains(&candidate).then_some(candidate))
    }

    /// Hold `dispatch` for `pause`. One that would create a mirror thread is
    /// registered as its conversation's provisioning.
    pub(crate) async fn hold(
        &self,
        dispatch: &Dispatch,
        pause: &HealthPause,
    ) -> Result<(), CoreError> {
        self.storage.hold_dispatch(dispatch, pause).await?;
        if dispatch.to.create_thread.is_some() {
            self.provisioning
                .lock()
                .unwrap()
                .entry(dispatch.conversation_id)
                .or_default()
                .pending
                .insert(dispatch.id);
        }
        Ok(())
    }

    /// Release what `pause` held: queue the dispatches held since `cutoff_ms`
    /// or later and return them, in order, to send. Older ones are dropped,
    /// except one that creates a mirror thread (later messages depend on it);
    /// messages stashed behind such a thread are dropped by the same cutoff.
    pub async fn release_held(
        &self,
        pause: &HealthPause,
        cutoff_ms: i64,
    ) -> Result<Released, CoreError> {
        let mut released = Released::default();
        let mut provisioned = Vec::new();
        for held in self.storage.release_held(pause).await? {
            let creates_thread = held.dispatch.to.create_thread.is_some();
            if held.held_at_ms < cutoff_ms && !creates_thread {
                released.dropped += 1;
                continue;
            }
            if creates_thread {
                provisioned.push(held.dispatch.conversation_id);
            }
            self.storage.enqueue_dispatch(&held.dispatch).await?;
            released.dispatches.push(held.dispatch);
        }
        for conversation_id in provisioned {
            let stashed = self
                .storage
                .drain_provisioning_stash(conversation_id)
                .await?;
            for (message, origin, title) in stashed {
                if message.timestamp_ms < cutoff_ms {
                    released.dropped += 1;
                    continue;
                }
                self.storage
                    .stash_provisioning_message(conversation_id, message.id, origin, &title)
                    .await?;
            }
        }
        Ok(released)
    }
}
