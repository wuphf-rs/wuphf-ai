//! Outbound dispatch execution: the core's message commands become WhatsApp
//! sends. WhatsApp has no threads, so dispatches never carry a thread target
//! and no delivery ever reports a provisioned thread.

use std::sync::Arc;

use whatsapp_rust::client::Client;
use whatsapp_rust::send::RevokeType;
use whatsapp_rust::wacore::proto_helpers::{MessageBuilderExt, build_quote_context_with_info};
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{AppStateError, Jid, SendError};
use wuphf_connector::{Delivery, Failure, Sender};
use wuphf_proto::connector as pb;

use crate::chat_state::{MuteCall, mute_call, now_ms};
use crate::disappearing::{TimerFallback, stamp};

pub struct WhatsAppSender {
    client: Arc<Client>,
    /// Set when the connector stamps disappearing timers: what to do with a
    /// dispatch that arrives without one.
    fallback: Option<TimerFallback>,
}

impl WhatsAppSender {
    pub fn new(client: Arc<Client>, fallback: Option<TimerFallback>) -> Self {
        Self { client, fallback }
    }
}

/// A target chat that does not parse can never succeed on retry, so it is
/// permanent and the core dead-letters the dispatch.
fn parse_chat(chat_id: &str) -> Result<Jid, Failure> {
    chat_id.parse().map_err(|error| {
        Failure::permanent(format!("parse WhatsApp target chat {chat_id:?}: {error}"))
    })
}

/// Attachments are dead-lettered rather than partially delivered: acking a
/// text-only copy would silently lose the media. A system note is the
/// exception: it is informational, so its text alone is sent and
/// acknowledged (an avatar note's photo is dropped).
fn check_attachments(dispatch: &pb::MessageCreateDispatch) -> Result<(), Failure> {
    if dispatch.attachments.is_empty() || dispatch.system_note {
        return Ok(());
    }
    Err(Failure::permanent(
        "attachments are not yet supported by wuphf-connector-whatsapp",
    ))
}

fn transient(error: SendError) -> Failure {
    Failure::Transient(anyhow::Error::new(error))
}

/// An invalid chat-action request (a malformed chat, a past mute end) can
/// never succeed on retry; anything else (encoding, keys, the send) may.
fn app_state_failure(error: AppStateError) -> Failure {
    match error {
        AppStateError::InvalidRequest(_) => Failure::permanent(error.to_string()),
        other => Failure::Transient(anyhow::Error::new(other)),
    }
}

impl Sender for WhatsAppSender {
    async fn send_message(&self, dispatch: pb::MessageCreateDispatch) -> Result<Delivery, Failure> {
        check_attachments(&dispatch)?;
        let chat = parse_chat(&dispatch.target_chat_id)?;
        let text = dispatch.text.unwrap_or_default();
        let mut message = match dispatch.reply_context {
            // A system note is plain text: no sender label, no reply.
            _ if dispatch.system_note => wa::Message::text(text),
            Some(reply) => wa::Message::text_with_context(text, self.quote_context(&chat, &reply)?),
            None => {
                if dispatch.reply_to_platform_message_id.is_some() {
                    return Err(Failure::permanent(
                        "outbound WhatsApp reply requires quoted-message context",
                    ));
                }
                wa::Message::text(text)
            }
        };
        // Every content send carries the chat's timer, so it disappears like
        // the rest of the chat instead of showing "will not disappear".
        let timer = match &self.fallback {
            Some(fallback) => {
                fallback
                    .resolve(
                        &self.client,
                        &dispatch.target_chat_id,
                        &chat,
                        dispatch.disappearing,
                    )
                    .await
            }
            None => dispatch.disappearing,
        };
        stamp(&mut message, timer.as_ref());
        let result = self
            .client
            .send_message(chat, message)
            .await
            .map_err(transient)?;
        Ok(Delivery {
            message_id: result.message_id,
            thread_id: None,
        })
    }

    /// WhatsApp keeps the original message id across an edit, so the id the
    /// edit API returns is the target id itself.
    async fn edit_message(&self, dispatch: pb::MessageEditDispatch) -> Result<String, Failure> {
        let chat = parse_chat(&dispatch.target_chat_id)?;
        let text = dispatch.text.unwrap_or_default();
        self.client
            .edit_message(
                chat,
                dispatch.target_platform_message_id.clone(),
                wa::Message::text(text),
            )
            .await
            .map_err(transient)
    }

    /// Bring the chat to the dispatched pin / mute / archive state, one chat
    /// action per field present. Emits nothing: WhatsApp's own app-state echo
    /// comes back through the event handlers, and the core finds it equal to
    /// what it stored. WhatsApp has no threads, so `target_thread_id` is
    /// never set here.
    async fn apply_chat_state(&self, dispatch: pb::ChatStateDispatch) -> Result<String, Failure> {
        let chat = parse_chat(&dispatch.target_chat_id)?;
        let actions = self.client.chat_actions();
        if let Some(pinned) = dispatch.pinned {
            let result = if pinned {
                actions.pin_chat(&chat).await
            } else {
                actions.unpin_chat(&chat).await
            };
            result.map_err(app_state_failure)?;
        }
        if let Some(mute) = dispatch.mute {
            let result = match mute_call(mute.muted_until, now_ms()) {
                MuteCall::Forever => actions.mute_chat(&chat).await,
                MuteCall::Until(end_ms) => actions.mute_chat_until(&chat, end_ms).await,
                MuteCall::Unmute => actions.unmute_chat(&chat).await,
            };
            result.map_err(app_state_failure)?;
        }
        if let Some(archived) = dispatch.archived {
            let result = if archived {
                actions.archive_chat(&chat, None).await
            } else {
                actions.unarchive_chat(&chat, None).await
            };
            result.map_err(app_state_failure)?;
        }
        Ok(dispatch.target_chat_id)
    }

    /// RevokeType::Sender revokes our own (bridge-authored) copy; the target
    /// id is returned unchanged for the acknowledgement.
    async fn delete_message(&self, dispatch: pb::MessageDeleteDispatch) -> Result<String, Failure> {
        let chat = parse_chat(&dispatch.target_chat_id)?;
        self.client
            .revoke_message(
                chat,
                dispatch.target_platform_message_id.clone(),
                RevokeType::Sender,
            )
            .await
            .map_err(transient)?;
        Ok(dispatch.target_platform_message_id)
    }
}

impl WhatsAppSender {
    /// Build the quote metadata for a reply dispatch. The quoted copy at this
    /// endpoint is always bridge-authored, since the router never dispatches a
    /// message back to its origin endpoint, so the quoted participant is our
    /// own account.
    fn quote_context(
        &self,
        chat: &Jid,
        reply: &pb::ReplyContext,
    ) -> Result<wa::ContextInfo, Failure> {
        let own = self.client.get_pn().ok_or_else(|| {
            Failure::permanent("outbound WhatsApp reply requires a paired account JID")
        })?;
        let own = own.to_non_ad();
        let quoted = wa::Message::text(reply.text.clone().unwrap_or_default());
        Ok(build_quote_context_with_info(
            reply.platform_message_id.clone(),
            &own,
            chat,
            chat,
            &quoted,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch(system_note: bool, attachments: usize) -> pb::MessageCreateDispatch {
        pb::MessageCreateDispatch {
            target_chat_id: "1555@s.whatsapp.net".into(),
            text: Some("🖼️ Photo changed".into()),
            attachments: (0..attachments)
                .map(|n| pb::Attachment {
                    media_id: format!("m{n}"),
                    ..Default::default()
                })
                .collect(),
            system_note,
            ..Default::default()
        }
    }

    #[test]
    fn a_note_with_a_photo_sends_its_text_and_is_not_dead_lettered() {
        assert!(check_attachments(&dispatch(true, 1)).is_ok());
        assert!(check_attachments(&dispatch(true, 0)).is_ok());
        assert!(check_attachments(&dispatch(false, 0)).is_ok());
        let error = check_attachments(&dispatch(false, 1)).unwrap_err();
        assert!(error.is_permanent());
    }

    #[test]
    fn an_invalid_chat_action_is_permanent() {
        assert!(app_state_failure(AppStateError::InvalidRequest("past".into())).is_permanent());
        assert!(
            !app_state_failure(AppStateError::Internal(anyhow::anyhow!("send failed")))
                .is_permanent()
        );
    }
}
