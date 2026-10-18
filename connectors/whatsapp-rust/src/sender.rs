//! Outbound dispatch execution: the core's message commands become WhatsApp
//! sends. WhatsApp has no threads, so dispatches never carry a thread target
//! and no delivery ever reports a provisioned thread.

use std::sync::Arc;

use whatsapp_rust::client::Client;
use whatsapp_rust::send::RevokeType;
use whatsapp_rust::wacore::proto_helpers::{MessageBuilderExt, build_quote_context_with_info};
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Jid, SendError};
use wuphf_connector::{Delivery, Failure, Sender};
use wuphf_proto::connector as pb;

pub struct WhatsAppSender {
    client: Arc<Client>,
}

impl WhatsAppSender {
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }
}

/// A target chat that does not parse can never succeed on retry, so it is
/// permanent and the core dead-letters the dispatch.
fn parse_chat(chat_id: &str) -> Result<Jid, Failure> {
    chat_id.parse().map_err(|error| {
        Failure::permanent(format!("parse WhatsApp target chat {chat_id:?}: {error}"))
    })
}

fn transient(error: SendError) -> Failure {
    Failure::Transient(anyhow::Error::new(error))
}

impl Sender for WhatsAppSender {
    async fn send_message(&self, dispatch: pb::MessageCreateDispatch) -> Result<Delivery, Failure> {
        // Attachments are dead-lettered rather than partially delivered: acking
        // a text-only copy would silently lose the media.
        if !dispatch.attachments.is_empty() {
            return Err(Failure::permanent(
                "attachments are not yet supported by wuphf-connector-whatsapp",
            ));
        }
        let chat = parse_chat(&dispatch.target_chat_id)?;
        let text = dispatch.text.unwrap_or_default();
        let message = match dispatch.reply_context {
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
