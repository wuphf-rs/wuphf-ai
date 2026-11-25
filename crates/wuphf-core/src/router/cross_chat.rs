//! Replies that quote a message of another conversation (plan 083): a group
//! message quoted in a member's DM topic ("Reply privately"), a story quoted
//! in its poster's DM topic.
//!
//! Such a parent has no copy at the reply's target, so the reply must never
//! wait for one. Within one connector the quote is kept, naming the chat the
//! quoted copy lives in; across connectors it is dropped. A message from a
//! chat whose replies are private (a story) may be quoted only in its
//! author's own chat: anywhere else the reply is refused, since it would show
//! the story to someone else.

use super::{Router, RouterError};
use crate::resolve::address_forms;
use wuphf_model::{ChatAddress, Endpoint, Message, ReplyContext};
use wuphf_storage::{ChatProfileStore, MessageStore, TopologyStore};

/// The refusal reason for a story quoted outside its poster's chat.
pub(crate) const STORY_QUOTED_ELSEWHERE: &str =
    "Not sent: a story can only be quoted in its poster's chat.";

/// How a reply quoting a parent of another conversation goes to one target.
pub(super) enum CrossChatQuote {
    /// With this quote.
    Quote(ReplyContext),
    /// Without a quote: the parent has no copy on the target's connector.
    Plain,
    /// Not at all, for this reason.
    Refuse(&'static str),
}

impl<S> Router<S>
where
    S: TopologyStore + MessageStore + ChatProfileStore,
{
    pub(super) async fn cross_chat_quote(
        &self,
        parent: &Message,
        target: &Endpoint,
    ) -> Result<CrossChatQuote, RouterError> {
        let Some(conversation) = self.storage.conversation(parent.conversation_id).await? else {
            return Ok(CrossChatQuote::Plain);
        };
        let private = self
            .storage
            .chat_profile(
                &conversation.source.connector_id,
                &conversation.source.address.chat_id,
            )
            .await?
            .is_some_and(|profile| profile.private_replies());
        if private && !self.is_authors_chat(parent, target).await? {
            return Ok(CrossChatQuote::Refuse(STORY_QUOTED_ELSEWHERE));
        }
        for endpoint_id in &conversation.endpoints {
            let endpoint = self.storage.endpoint(*endpoint_id).await?;
            if endpoint.connector_id != target.connector_id {
                continue;
            }
            let Some(copy) = self
                .storage
                .copy_at_endpoint(parent.id, endpoint.id)
                .await?
            else {
                continue;
            };
            let authored_by_bridge = self
                .storage
                .is_sent_copy(endpoint.id, &copy.native_id)
                .await?;
            let mut quote =
                ReplyContext::quoting(parent.clone(), copy.native_id, authored_by_bridge);
            if endpoint.chat_id != target.chat_id {
                quote.chat_id = Some(endpoint.chat_id);
            }
            return Ok(CrossChatQuote::Quote(quote));
        }
        Ok(CrossChatQuote::Plain)
    }

    /// Whether `target` is the direct chat of the parent's author, under any
    /// address form known for it.
    async fn is_authors_chat(
        &self,
        parent: &Message,
        target: &Endpoint,
    ) -> Result<bool, RouterError> {
        let author = parent
            .participant
            .as_deref()
            .filter(|participant| !participant.is_empty())
            .unwrap_or(&parent.author_id);
        let forms = address_forms(
            &*self.storage,
            &target.connector_id,
            &ChatAddress::new(target.chat_id.clone(), None),
        )
        .await?;
        Ok(forms.iter().any(|form| form.chat_id == author))
    }
}
