//! Inbound normalization: WhatsApp messages become the core's normalized
//! event frames. The connector stays thin — no routing or bridge decisions
//! here, only platform-object to event mapping, matching the semantics of the
//! Go whatsmeow connector (`connectors/whatsapp/internal/whatsapp/updates.go`).

use tokio::sync::mpsc;
use tracing::warn;
use whatsapp_rust::bot::MessageContext;
use whatsapp_rust::wacore::proto_helpers::MessageExt;
use whatsapp_rust::waproto::whatsapp as wa;
use wuphf_proto::connector as pb;

/// Maps inbound WhatsApp messages onto core event frames and queues them for
/// the session loop. Cheap to clone: every handle shares the one frame sink.
#[derive(Clone)]
pub struct Normalizer {
    inputs: mpsc::Sender<pb::ClientFrame>,
}

impl Normalizer {
    pub fn new(inputs: mpsc::Sender<pb::ClientFrame>) -> Self {
        Self { inputs }
    }

    pub async fn handle(&self, ctx: MessageContext) {
        let Some(frame) = normalize(&ctx) else {
            return;
        };
        if self.inputs.send(frame).await.is_err() {
            warn!("core session input closed; dropping inbound WhatsApp event");
        }
    }
}

/// The device part of a JID is always stripped so a per-device address never
/// splits a chat or an author into two identities.
fn canonical(jid: &whatsapp_rust::Jid) -> String {
    jid.to_non_ad().to_string()
}

fn normalize(ctx: &MessageContext) -> Option<pb::ClientFrame> {
    let info = &ctx.info;
    let message = ctx.message.as_ref();
    let chat = canonical(&info.source.chat);

    if let Some(protocol) = message.protocol_message.as_option() {
        return match protocol.r#type {
            // WhatsApp carries an edit as a protocol message whose key names the
            // original message and whose edited body holds the new content; the
            // core suppresses no-op and echoed edits by comparing text.
            Some(wa::message::protocol_message::Type::MESSAGE_EDIT) => {
                let original_id = protocol.key.as_option()?.id.clone()?;
                if original_id.is_empty() {
                    return None;
                }
                Some(frame(pb::client_frame::Body::MessageEdit(
                    pb::MessageEditEvent {
                        chat_id: chat,
                        thread_id: None,
                        platform_message_id: original_id,
                        text: protocol
                            .edited_message
                            .as_option()
                            .and_then(|edited| edited.text_content().map(str::to_owned)),
                    },
                )))
            }
            // A delete arrives as a REVOKE protocol message naming the revoked
            // message; the core resolves it and fans the deletion out.
            Some(wa::message::protocol_message::Type::REVOKE) => {
                let revoked_id = protocol.key.as_option()?.id.clone()?;
                if revoked_id.is_empty() {
                    return None;
                }
                Some(frame(pb::client_frame::Body::MessageDelete(
                    pb::MessageDeleteEvent {
                        chat_id: chat,
                        thread_id: None,
                        platform_message_id: revoked_id,
                    },
                )))
            }
            _ => None,
        };
    }

    // A reaction names the reacted message and holds the emoji (empty text
    // clears the reactor's reaction); the core fans it to the other endpoints.
    if let Some(reaction) = message.reaction_message.as_option() {
        let target = reaction.key.as_option()?.id.clone()?;
        if target.is_empty() {
            return None;
        }
        return Some(frame(pb::client_frame::Body::Reaction(pb::ReactionEvent {
            chat_id: chat,
            thread_id: None,
            platform_message_id: target,
            sender: canonical(&info.source.sender),
            sender_name: sender_name(ctx),
            emoji: reaction.text.clone().unwrap_or_default(),
            from_me: info.source.is_from_me,
        })));
    }

    let Some(text) = message.text_content() else {
        // Media messages are not bridged yet; dropping is deliberate rather
        // than delivering a caption that silently loses the attachment.
        if has_attachment(message) {
            warn!(
                chat,
                message_id = %info.id,
                "dropping WhatsApp media message: attachments are not yet supported"
            );
        }
        return None;
    };
    let sender = canonical(&info.source.sender);
    Some(frame(pb::client_frame::Body::MessageCreate(
        pb::MessageCreateEvent {
            chat_id: chat,
            thread_id: None,
            // The core labels freshly provisioned mirror threads from the chat
            // title; contact/group name resolution is not wired up yet.
            chat_title: None,
            platform_message_id: info.id.clone(),
            // The participant labels every chat kind, not only groups: without
            // it, a direct chat's two sides are indistinguishable on a mirror.
            participant: Some(sender.clone()),
            sender,
            text: Some(text.to_owned()),
            attachments: vec![],
            reply_to_platform_message_id: reply_target(message),
            timestamp_ms: info.timestamp.timestamp_millis(),
            sender_name: sender_name(ctx),
        },
    )))
}

/// The per-message sender label. The account owner's own messages are labelled
/// "Me" so a direct chat's two sides stay distinguishable on the mirror; any
/// sender whose device is not the primary phone is tagged as writing from a
/// linked device.
fn sender_name(ctx: &MessageContext) -> Option<String> {
    let info = &ctx.info;
    let name = if info.source.is_from_me {
        "Me".to_owned()
    } else {
        info.push_name.trim().to_owned()
    };
    if name.is_empty() {
        return None;
    }
    if info.source.sender.device != 0 {
        return Some(format!("{name} (linked device)"));
    }
    Some(name)
}

fn reply_target(message: &wa::Message) -> Option<String> {
    let stanza_id = context_info(message)?.stanza_id.clone()?;
    (!stanza_id.is_empty()).then_some(stanza_id)
}

fn context_info(message: &wa::Message) -> Option<&wa::ContextInfo> {
    message
        .extended_text_message
        .as_option()
        .and_then(|m| m.context_info.as_option())
        .or_else(|| {
            message
                .image_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            message
                .video_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            message
                .document_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            message
                .audio_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            message
                .sticker_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
}

fn has_attachment(message: &wa::Message) -> bool {
    message.image_message.as_option().is_some()
        || message.video_message.as_option().is_some()
        || message.document_message.as_option().is_some()
        || message.audio_message.as_option().is_some()
        || message.sticker_message.as_option().is_some()
}

fn frame(body: pb::client_frame::Body) -> pb::ClientFrame {
    pb::ClientFrame { body: Some(body) }
}
