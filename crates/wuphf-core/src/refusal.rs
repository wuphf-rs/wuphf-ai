//! Receive-only chats (plan 083): what the bridge does not send into a
//! conversation's source chat, and the note that says so on the copy the owner
//! wrote.
//!
//! A chat is receive-only when its connector reports that this account may
//! not post there (a story feed, an admin-only group as a member, a channel it
//! does not own) or when the owner marked it (`receive_only`). New messages
//! and edits into it are refused; reactions pass unless the connector reports
//! the chat takes none. A delete into a source connector the owner listed in
//! `mirror_deletes_kept` is dropped silently: the message stays there.
//!
//! Every routed dispatch goes through [`Core::screen`] before it is saved, so
//! no path (a new message, an edit, a reaction, a replayed stash) can slip
//! past it.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use wuphf_model::{
    Action, ChatRef, Conversation, ConversationId, Dispatch, EndpointId, MessageId, Target,
};
use wuphf_storage::{MessageStore, Storage, StorageError, TopologyStore};

use crate::{Core, CoreError, RoutingConfig};

/// How long the refusal note stays before the connector deletes it.
pub(crate) const REFUSAL_NOTE_TTL_SECS: u32 = 30;

/// What a dispatch into a conversation's source chat may do, worked out once
/// per conversation in a batch.
#[derive(Clone)]
pub(crate) struct SourcePolicy {
    accepts_messages: bool,
    accepts_reactions: bool,
    keeps_deletes: bool,
    title: String,
}

/// What [`Core::screen`] decided for one dispatch.
pub(crate) enum Screened {
    /// Send it.
    Pass(Dispatch),
    /// Drop it; when the owner's copy is known, tell them why with this
    /// refusal dispatch.
    Refused(Option<Dispatch>),
    /// Drop it without a word (a delete the source keeps).
    Dropped,
}

impl<S> Core<S>
where
    S: Storage,
{
    /// Decide whether `dispatch` may go. `policies` caches each
    /// conversation's source policy during one batch.
    pub(crate) async fn screen(
        &self,
        routing: &Arc<RoutingConfig>,
        dispatch: Dispatch,
        policies: &mut HashMap<ConversationId, Option<(Conversation, SourcePolicy)>>,
    ) -> Result<Screened, CoreError> {
        if !matches!(
            dispatch.action,
            Action::Send { .. }
                | Action::Edit { .. }
                | Action::React { .. }
                | Action::Delete { .. }
        ) {
            return Ok(Screened::Pass(dispatch));
        }
        let policy = match policies.entry(dispatch.conversation_id) {
            Entry::Occupied(known) => known.into_mut(),
            Entry::Vacant(unknown) => {
                let policy = match self.storage.conversation(dispatch.conversation_id).await? {
                    Some(conversation) => {
                        let policy = self.source_policy(routing, &conversation).await?;
                        Some((conversation, policy))
                    }
                    None => None,
                };
                unknown.insert(policy)
            }
        };
        let Some((conversation, policy)) = policy.clone() else {
            return Ok(Screened::Pass(dispatch));
        };
        if !into_source(&conversation, &dispatch.to) {
            return Ok(Screened::Pass(dispatch));
        }
        match &dispatch.action {
            Action::Send { message, .. } | Action::Edit { message, .. }
                if !policy.accepts_messages =>
            {
                let reason = format!("Not sent: {} is receive-only.", policy.title);
                Ok(Screened::Refused(
                    self.refusal_for(message.id, &dispatch, reason).await?,
                ))
            }
            Action::React { .. } if !policy.accepts_reactions => Ok(Screened::Refused(None)),
            Action::Delete { .. } if policy.keeps_deletes => Ok(Screened::Dropped),
            _ => Ok(Screened::Pass(dispatch)),
        }
    }

    /// The source policy of `conversation`: what its connector reported about
    /// the source chat, and what the owner set.
    async fn source_policy(
        &self,
        routing: &Arc<RoutingConfig>,
        conversation: &Conversation,
    ) -> Result<SourcePolicy, CoreError> {
        let profile = self.source_profile(conversation).await?;
        let mut endpoints = Vec::new();
        for endpoint_id in &conversation.endpoints {
            let endpoint = self.storage.endpoint(*endpoint_id).await?;
            endpoints.push(ChatRef {
                connector_id: endpoint.connector_id.clone(),
                address: endpoint.address(),
            });
        }
        let marked = routing.marked_receive_only(conversation, &endpoints);
        Ok(SourcePolicy {
            accepts_messages: !marked
                && profile
                    .as_ref()
                    .is_none_or(|profile| profile.accepts_messages()),
            accepts_reactions: profile
                .as_ref()
                .is_none_or(|profile| profile.accepts_reactions()),
            keeps_deletes: routing.keeps_mirror_deletes(&conversation.source.connector_id),
            title: conversation
                .title
                .clone()
                .unwrap_or_else(|| conversation.source.address.chat_id.clone()),
        })
    }

    /// The refusal of `refused` (a dispatch of message `message_id`).
    async fn refusal_for(
        &self,
        message_id: MessageId,
        refused: &Dispatch,
        reason: String,
    ) -> Result<Option<Dispatch>, CoreError> {
        Ok(refusal_dispatch(
            &*self.storage,
            message_id,
            refused.conversation_id,
            refused.to.endpoint_id,
            reason,
        )
        .await?)
    }
}

/// The refusal of a message the bridge did not send to `refused_at`, sent to
/// the copy the message was received as: the owner's own message in the
/// mirror. `None` when that copy is unknown (an owner send from the TUI) or is
/// the refused endpoint itself.
pub(crate) async fn refusal_dispatch<S: MessageStore + TopologyStore>(
    storage: &S,
    message_id: MessageId,
    conversation_id: ConversationId,
    refused_at: EndpointId,
    reason: String,
) -> Result<Option<Dispatch>, StorageError> {
    let Some(origin) = storage.received_copy(message_id).await? else {
        return Ok(None);
    };
    if origin.endpoint_id == refused_at {
        return Ok(None);
    }
    let endpoint = storage.endpoint(origin.endpoint_id).await?;
    Ok(Some(Dispatch::new(
        Target {
            endpoint_id: origin.endpoint_id,
            connector_id: endpoint.connector_id.clone(),
            address: endpoint.address(),
            create_thread: None,
        },
        conversation_id,
        Action::Refuse {
            message_id,
            native_id: origin.native_id,
            reason,
            note_ttl_secs: REFUSAL_NOTE_TTL_SECS,
        },
        None,
    )))
}

/// Whether `target` is the conversation's source chat (as plan 079's health
/// pauses tell the back direction).
fn into_source(conversation: &Conversation, target: &Target) -> bool {
    conversation.source.connector_id == target.connector_id
        && conversation.source.address.chat_id == target.address.chat_id
}
