//! Stories (plan 084): messages of a feed chat, one whose connector reports
//! that replies to its messages are private (WhatsApp Status).
//!
//! The owner can stop mirroring them all (`stories.mirror`) or someone's
//! (`stories.muted`): those are dropped before they are stored. Every story
//! that is stored gets an expiry 24 hours after it was posted; with
//! `stories.expire_copies` on, [`Core::expire_stories`] deletes its mirror
//! copies then. The source is left alone: WhatsApp expires the story itself.

use wuphf_model::{
    Action, ChatAddress, Dispatch, Endpoint, Message, MessageId, NewMessage, Target,
};
use wuphf_storage::Storage;

use crate::resolve::address_forms;
use crate::{Core, CoreError, RoutingConfig};

/// How long a story lasts.
pub const STORY_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;

/// How many due stories one sweep handles.
const SWEEP_LIMIT: usize = 200;

impl<S> Core<S>
where
    S: Storage,
{
    /// Whether `origin` is a feed chat: its messages are stories.
    pub(crate) async fn is_feed(&self, origin: &Endpoint) -> Result<bool, CoreError> {
        Ok(self
            .storage
            .chat_profile(&origin.connector_id, &origin.chat_id)
            .await?
            .is_some_and(|profile| profile.private_replies()))
    }

    /// Whether a story the owner does not mirror: all are off, or its author
    /// is muted (under any address form of their direct chat).
    pub(crate) async fn story_muted(
        &self,
        routing: &RoutingConfig,
        origin: &Endpoint,
        story: &NewMessage,
    ) -> Result<bool, CoreError> {
        let stories = routing.stories();
        if !stories.mirror {
            return Ok(true);
        }
        let author = story
            .participant
            .as_deref()
            .filter(|participant| !participant.is_empty())
            .unwrap_or(&story.author_id);
        for muted in &stories.muted {
            if muted.connector_id != origin.connector_id {
                continue;
            }
            let forms = address_forms(
                &*self.storage,
                &muted.connector_id,
                &ChatAddress::new(muted.address.chat_id.clone(), None),
            )
            .await?;
            if forms.iter().any(|form| form.chat_id == author) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Note when a stored story's mirror copies are due to go.
    pub(crate) async fn record_story(&self, story: &Message) -> Result<(), CoreError> {
        self.storage
            .record_expiry(story.id, story.timestamp_ms + STORY_LIFETIME_MS)
            .await?;
        Ok(())
    }

    /// Delete the mirror copies of the stories that expired by `now_ms`, when
    /// the owner turned `stories.expire_copies` on; off, nothing is done and
    /// the expiries wait. Returns the deletes to deliver.
    pub async fn expire_stories(&self, now_ms: i64) -> Result<Vec<Dispatch>, CoreError> {
        let routing = self.routing.routing();
        if !routing.stories().expire_copies {
            return Ok(Vec::new());
        }
        let mut deletes = Vec::new();
        for id in self.storage.due_expiries(now_ms, SWEEP_LIMIT).await? {
            deletes.extend(self.expire_story(id).await?);
            self.storage.forget_expiry(id).await?;
        }
        Ok(self.save_dispatches(&routing, deletes).await?.0)
    }

    /// The deletes of one expired story's mirror copies (every copy but the
    /// source's). The story is marked deleted, so the mirror's echo of the
    /// delete is redundant.
    async fn expire_story(&self, id: MessageId) -> Result<Vec<Dispatch>, CoreError> {
        let Some(mut story) = self.storage.message(id).await? else {
            return Ok(Vec::new());
        };
        if story.deleted {
            return Ok(Vec::new());
        }
        let Some(conversation) = self.storage.conversation(story.conversation_id).await? else {
            return Ok(Vec::new());
        };
        let source = self
            .storage
            .resolve_endpoint(
                &conversation.source.connector_id,
                &conversation.source.address,
            )
            .await?;
        let mut deletes = Vec::new();
        for endpoint_id in &conversation.endpoints {
            if Some(*endpoint_id) == source {
                continue;
            }
            let Some(copy) = self.storage.copy_at_endpoint(id, *endpoint_id).await? else {
                continue;
            };
            let endpoint = self.storage.endpoint(*endpoint_id).await?;
            deletes.push(Dispatch::new(
                Target {
                    endpoint_id: endpoint.id,
                    connector_id: endpoint.connector_id.clone(),
                    address: endpoint.address(),
                    create_thread: None,
                },
                conversation.id,
                Action::Delete {
                    message_id: id,
                    native_id: copy.native_id,
                },
                None,
            ));
        }
        story.deleted = true;
        self.storage.put_message(story).await?;
        Ok(deletes)
    }
}
