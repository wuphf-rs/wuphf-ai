//! Inbound normalization: WhatsApp messages become the core's normalized
//! event frames. The connector stays thin — no routing or bridge decisions
//! here, only platform-object to event mapping, matching the semantics of the
//! Go whatsmeow connector (`connectors/whatsapp/internal/whatsapp/updates.go`).

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;
use whatsapp_rust::bot::MessageContext;
use whatsapp_rust::client::Client;
use whatsapp_rust::types::message::MessageInfo;
use whatsapp_rust::wacore::proto_helpers::MessageExt;
use whatsapp_rust::waproto::whatsapp as wa;
use wuphf_proto::connector as pb;

use crate::chats::chat_forms;
use crate::disappearing::{ephemeral_setting, message_timer};
use crate::names::ChatTitles;

/// Maps inbound WhatsApp messages onto core event frames and queues them for
/// the session loop. Cheap to clone: every handle shares the one frame sink.
#[derive(Clone)]
pub struct Normalizer {
    inputs: mpsc::Sender<pb::ClientFrame>,
    titles: Arc<ChatTitles>,
}

impl Normalizer {
    pub fn new(inputs: mpsc::Sender<pb::ClientFrame>, titles: Arc<ChatTitles>) -> Self {
        Self { inputs, titles }
    }

    pub async fn handle(&self, ctx: MessageContext) {
        let title = self.titles.title(&ctx.client, &ctx.info).await;
        let Some(mut frame) = normalize(&ctx.info, &ctx.message, title) else {
            return;
        };
        // A DM's timer is reported under both its LID and phone forms: the core
        // has no link between them for this connector.
        if let Some(pb::client_frame::Body::ChatUpdate(event)) = &mut frame.body {
            with_other_forms(&ctx.client, &ctx.info, event).await;
        }
        if self.inputs.send(frame).await.is_err() {
            warn!("core session input closed; dropping inbound WhatsApp event");
        }
    }
}

/// Add the chat's other address forms to a chat update about it.
async fn with_other_forms(client: &Client, info: &MessageInfo, event: &mut pb::ChatUpdateEvent) {
    let Some(update) = event.updates.first().cloned() else {
        return;
    };
    for form in chat_forms(client, &info.source.chat).await {
        if form != update.chat_id {
            event.updates.push(pb::ChatUpdate {
                chat_id: form,
                ..update.clone()
            });
        }
    }
}

/// The device part of a JID is always stripped so a per-device address never
/// splits a chat or an author into two identities.
pub fn canonical(jid: &whatsapp_rust::Jid) -> String {
    jid.to_non_ad().to_string()
}

/// One inbound message as a core frame, or `None` when it is not bridged.
/// `chat_title` is the chat's title as the namer resolved it.
fn normalize(
    info: &MessageInfo,
    message: &wa::Message,
    chat_title: Option<String>,
) -> Option<pb::ClientFrame> {
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
            // A timer change arrives as an EPHEMERAL_SETTING protocol message (0
            // turns it off); it updates the chat, not a message.
            Some(wa::message::protocol_message::Type::EPHEMERAL_SETTING) => Some(frame(
                pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
                    updates: vec![ephemeral_setting(chat, info, protocol)],
                    snapshot: false,
                }),
            )),
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
            sender_name: sender_name(info),
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
            // The core names a freshly provisioned mirror thread from the chat
            // title, and renames one still named with the raw id.
            chat_title,
            platform_message_id: info.id.clone(),
            // The participant labels every chat kind, not only groups: without
            // it, a direct chat's two sides are indistinguishable on a mirror.
            participant: Some(sender.clone()),
            sender,
            text: Some(text.to_owned()),
            attachments: vec![],
            reply_to_platform_message_id: reply_target(message),
            timestamp_ms: info.timestamp.timestamp_millis(),
            sender_name: sender_name(info),
            disappearing: message_timer(info, message),
        },
    )))
}

/// The per-message sender label. The account owner's own messages are labelled
/// "Me" so a direct chat's two sides stay distinguishable on the mirror; any
/// sender whose device is not the primary phone is tagged as writing from a
/// linked device.
fn sender_name(info: &MessageInfo) -> Option<String> {
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

pub fn context_info(message: &wa::Message) -> Option<&wa::ContextInfo> {
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

pub fn frame(body: pb::client_frame::Body) -> pb::ClientFrame {
    pb::ClientFrame { body: Some(body) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::chrono::DateTime;
    use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
    use whatsapp_rust::waproto::buffa::MessageField;

    fn info(ephemeral_expiration: Option<u32>) -> MessageInfo {
        let mut info = MessageInfo {
            id: "wa-1".into(),
            push_name: "Alice".into(),
            timestamp: DateTime::from_timestamp(1_700_000_500, 0).unwrap(),
            ephemeral_expiration,
            ..Default::default()
        };
        info.source.chat = "1555@s.whatsapp.net".parse().unwrap();
        info.source.sender = "1555@s.whatsapp.net".parse().unwrap();
        info
    }

    fn create(frame: Option<pb::ClientFrame>) -> pb::MessageCreateEvent {
        match frame.and_then(|frame| frame.body) {
            Some(pb::client_frame::Body::MessageCreate(event)) => event,
            other => panic!("expected a create, got {other:?}"),
        }
    }

    fn chat_update(frame: Option<pb::ClientFrame>) -> pb::ChatUpdateEvent {
        match frame.and_then(|frame| frame.body) {
            Some(pb::client_frame::Body::ChatUpdate(event)) => event,
            other => panic!("expected a chat update, got {other:?}"),
        }
    }

    fn timed_text() -> wa::Message {
        wa::Message::text_with_context(
            "hi",
            wa::ContextInfo {
                expiration: Some(86_400),
                ephemeral_setting_timestamp: Some(1_700_000_000),
                ..Default::default()
            },
        )
    }

    #[test]
    fn a_create_carries_the_message_timer() {
        let event = create(normalize(&info(Some(86_400)), &timed_text(), None));
        assert_eq!(
            event.disappearing,
            Some(pb::DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: Some(1_700_000_000),
            })
        );
        for none in [Some(0), None] {
            let event = create(normalize(&info(none), &timed_text(), None));
            assert_eq!(event.disappearing, None);
        }
    }

    #[test]
    fn the_resolved_title_names_the_chat() {
        let message = wa::Message::text("hi");
        let titled = create(normalize(
            &info(None),
            &message,
            Some("[P] Alice (1555)".into()),
        ));
        assert_eq!(titled.chat_title.as_deref(), Some("[P] Alice (1555)"));
        let untitled = create(normalize(&info(None), &message, None));
        assert_eq!(untitled.chat_title, None);
    }

    fn ephemeral_setting_message(seconds: u32, setting_timestamp: Option<i64>) -> wa::Message {
        wa::Message {
            protocol_message: MessageField::some(wa::message::ProtocolMessage {
                r#type: Some(wa::message::protocol_message::Type::EPHEMERAL_SETTING),
                ephemeral_expiration: Some(seconds),
                ephemeral_setting_timestamp: setting_timestamp,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_timer_setting_updates_the_chat() {
        for seconds in [604_800, 0] {
            let event = chat_update(normalize(
                &info(None),
                &ephemeral_setting_message(seconds, Some(1_700_000_000)),
                None,
            ));
            assert_eq!(
                event.updates,
                [pb::ChatUpdate {
                    chat_id: "1555@s.whatsapp.net".into(),
                    disappearing: Some(pb::DisappearingTimer {
                        seconds,
                        setting_timestamp: Some(1_700_000_000),
                    }),
                    ..Default::default()
                }]
            );
        }
        // Without its own timestamp, the setting dates from the message.
        let event = chat_update(normalize(
            &info(None),
            &ephemeral_setting_message(86_400, None),
            None,
        ));
        assert_eq!(
            event.updates[0].disappearing,
            Some(pb::DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: Some(1_700_000_500),
            })
        );
    }
}
