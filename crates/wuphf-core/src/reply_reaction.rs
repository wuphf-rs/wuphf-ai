//! The owner's lone-emoji reply is a reaction (plan 081). Telegram offers a
//! fixed set of reaction emoji and WhatsApp takes any, so a reply whose whole
//! text is one emoji becomes the owner's reaction on the replied-to message
//! instead of a message.
//!
//! The reply is stored with its received copy like any message, so a later
//! edit or delete resolves to it, but it is never routed. Its reaction is a
//! row in the reaction store on the parent's copy at the origin, keyed
//! `reply:<reply native id>`: the row is also what marks the message as a
//! reply reaction. Deleting the reply clears the reaction; editing it to
//! another lone emoji changes it, and to anything else clears it (the reply
//! is then an ordinary message that was never sent). Each reaction dispatch
//! names the reply, which gets a delivered signal once it is acknowledged.

use crate::ingest::{REPLY_KEY_PREFIX, ReactionChange};
use crate::{Core, CoreError, RoutingConfig};
use std::sync::Arc;
use wuphf_model::{
    Action, Dispatch, Endpoint, EndpointId, IgnoreReason, IngestOutcome, Message, MessageCopyRef,
    MessageId, ReceiptKind, Target,
};
use wuphf_storage::Storage;

/// The emoji `text` is, fully qualified, when it is exactly one emoji
/// (surrounding whitespace aside).
pub(crate) fn lone_emoji(text: &str) -> Option<&'static str> {
    emojis::get(text.trim()).map(|emoji| emoji.as_str())
}

impl<S> Core<S>
where
    S: Storage,
{
    /// The emoji a new message reacts with, when it is the owner's reply to
    /// a stored message whose whole text is one emoji. `author_account` is
    /// the author as `owner_ids` name accounts, when the connector said.
    pub(crate) fn reply_reaction_emoji(
        &self,
        routing: &RoutingConfig,
        origin: &Endpoint,
        message: &Message,
        author_account: Option<&str>,
    ) -> Option<&'static str> {
        message.reply_to?;
        if !message.attachments.is_empty() {
            return None;
        }
        let emoji = lone_emoji(message.text.as_deref()?)?;
        let author = author_account.unwrap_or(&message.author_id);
        routing
            .is_owner(&origin.connector_id, author, false)
            .then_some(emoji)
    }

    /// The parent a stored message reacts to, when it is a reply reaction.
    pub(crate) async fn reply_reaction_parent(
        &self,
        origin: EndpointId,
        message: &Message,
        native_id: &str,
    ) -> Result<Option<MessageId>, CoreError> {
        let Some(parent) = message.reply_to else {
            return Ok(None);
        };
        let key = format!("{REPLY_KEY_PREFIX}{native_id}");
        Ok(self
            .stored_reaction(parent, origin, &key)
            .await?
            .map(|_| parent))
    }

    /// Set the reply `reply` (native id `reply_native_id` at `origin`) as the
    /// owner's reaction on `parent`, or clear it with `None`.
    pub(crate) async fn ingest_reply_reaction(
        &self,
        routing: &Arc<RoutingConfig>,
        origin: &Endpoint,
        parent: MessageId,
        reply: &Message,
        reply_native_id: &str,
        emoji: Option<&str>,
    ) -> Result<IngestOutcome, CoreError> {
        let change = ReactionChange {
            reactor: reply.author_id.clone(),
            reactor_name: reply.author_name.clone(),
            emoji: emoji.map(str::to_owned),
            from_me: false,
            via_reply: Some(reply_native_id.to_owned()),
        };
        if self
            .stored_reaction(parent, origin.id, &change.key())
            .await?
            .as_deref()
            == emoji
        {
            return Ok(IngestOutcome::Ignored(IgnoreReason::RedundantReaction));
        }
        let parent = self
            .storage
            .message(parent)
            .await?
            .ok_or(CoreError::ReactionTargetNotStored(parent))?;
        let mut routed = self
            .reaction_changed(routing, origin, &parent, change)
            .await?;
        if emoji.is_some() {
            for dispatch in &mut routed {
                if let Action::React { confirm, .. } = &mut dispatch.action {
                    *confirm = Some(MessageCopyRef {
                        endpoint_id: origin.id,
                        native_id: reply_native_id.to_owned(),
                    });
                }
            }
        }
        let (dispatches, blocked) = self.save_dispatches(routing, routed).await?;
        Ok(IngestOutcome::Routed {
            message_id: reply.id,
            dispatches,
            blocked,
        })
    }

    /// The delivered signal for the reply an acknowledged reaction came
    /// from, queued like any dispatch.
    pub(crate) async fn reply_confirmation(
        &self,
        dispatch: &Dispatch,
    ) -> Result<Option<Dispatch>, CoreError> {
        let Action::React {
            message_id,
            confirm: Some(reply),
            ..
        } = &dispatch.action
        else {
            return Ok(None);
        };
        let endpoint = self.storage.endpoint(reply.endpoint_id).await?;
        let signal = Dispatch::new(
            Target {
                endpoint_id: endpoint.id,
                connector_id: endpoint.connector_id.clone(),
                address: endpoint.address(),
                create_thread: None,
            },
            dispatch.conversation_id,
            Action::Receipt {
                message_id: *message_id,
                native_id: reply.native_id.clone(),
                kind: ReceiptKind::Delivered,
            },
            None,
        );
        self.storage.enqueue_dispatch(&signal).await?;
        Ok(Some(signal))
    }
}

#[cfg(test)]
mod tests {
    use super::lone_emoji;

    #[test]
    fn a_lone_emoji_is_recognised_in_any_form() {
        assert_eq!(lone_emoji("🫠"), Some("🫠"));
        assert_eq!(lone_emoji("  🥲\n"), Some("🥲"));
        // Unqualified, without U+FE0F.
        assert_eq!(lone_emoji("\u{2764}"), Some("❤️"));
        // A skin tone and a ZWJ sequence are one emoji each.
        assert_eq!(lone_emoji("👍🏽"), Some("👍🏽"));
        assert_eq!(lone_emoji("👩‍💻"), Some("👩‍💻"));
        assert_eq!(lone_emoji("🇮🇳"), Some("🇮🇳"));
    }

    #[test]
    fn anything_more_is_not_a_lone_emoji() {
        for text in ["", "ok", "🫠🫠", "🫠 nice", "1", "#"] {
            assert_eq!(lone_emoji(text), None, "{text:?}");
        }
    }
}
