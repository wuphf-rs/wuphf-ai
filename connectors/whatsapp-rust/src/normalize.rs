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
use whatsapp_rust::{Jid, Server};
use wuphf_proto::connector as pb;

use crate::avatars::Avatars;
use crate::chats::{Sightings, chat_forms, sighting_updates};
use crate::disappearing::{
    CHAT_UPDATE_BATCH, chat_update_frames, ephemeral_setting, message_timer,
};
use crate::names::ChatTitles;

/// Maps inbound WhatsApp messages onto core event frames and queues them for
/// the session loop. Cheap to clone: every handle shares the one frame sink.
#[derive(Clone)]
pub struct Normalizer {
    inputs: mpsc::Sender<pb::ClientFrame>,
    titles: Arc<ChatTitles>,
    avatars: Arc<Avatars>,
    sightings: Arc<Sightings>,
}

/// The names a message needs, resolved from the contacts table before it is
/// normalized.
#[derive(Debug, Default)]
struct Names {
    /// The sender's display name (not for the owner's own messages).
    sender: Option<String>,
    /// `(user part, display name)` for each mentioned person with a known name.
    mentions: Vec<(String, String)>,
}

impl Normalizer {
    pub fn new(
        inputs: mpsc::Sender<pb::ClientFrame>,
        titles: Arc<ChatTitles>,
        avatars: Arc<Avatars>,
    ) -> Self {
        Self {
            inputs,
            titles,
            avatars,
            sightings: Arc::new(Sightings::default()),
        }
    }

    pub async fn handle(&self, ctx: MessageContext) {
        let (client, info) = (&*ctx.client, &ctx.info);
        // Before the title is resolved, so the newest push name names the chat.
        // The owner's own messages carry the owner's push name: never stored.
        if !info.source.is_from_me && is_person(&info.source.sender) {
            self.titles
                .contacts()
                .upsert_push_name(canonical(&info.source.sender), &info.push_name)
                .await;
        }
        let title = self.titles.title(client, info).await;
        let names = Names {
            sender: self.sender_display_name(client, info).await,
            mentions: self.mentions(client, &ctx.message).await,
        };
        let Some(mut frame) = normalize(info, &ctx.message, title.clone(), &names) else {
            return;
        };
        // A DM's timer is reported under both its LID and phone forms: the core
        // has no link between them for this connector.
        if let Some(pb::client_frame::Body::ChatUpdate(event)) = &mut frame.body {
            with_other_forms(client, info, event).await;
        }
        self.report_first_sighting(client, info, title).await;
        if self.inputs.send(frame).await.is_err() {
            warn!("core session input closed; dropping inbound WhatsApp event");
        }
    }

    /// The first time this process sees a chat, one snapshot of what is known
    /// about it (title, a group's description and member count, the picture),
    /// before the message: it gives a new mirror thread's info card its
    /// content. Sent from here, an event handler, never from the sender.
    async fn report_first_sighting(
        &self,
        client: &Client,
        info: &MessageInfo,
        title: Option<String>,
    ) {
        let chat = info.source.chat.to_non_ad();
        if !matches!(chat.server, Server::Pn | Server::Lid | Server::Group)
            || !self.sightings.first(&canonical(&chat))
        {
            return;
        }
        let (forms, facts) = if chat.server == Server::Group {
            let facts = self.titles.groups().facts(client, &chat).await;
            (vec![canonical(&chat)], facts)
        } else {
            (chat_forms(client, &chat).await, None)
        };
        let avatar = self.avatars.fetch(client, &chat).await;
        let updates = sighting_updates(forms, title, facts.as_ref(), avatar);
        for frame in chat_update_frames(updates, CHAT_UPDATE_BATCH, true) {
            if self.inputs.send(frame).await.is_err() {
                warn!("core session input closed; dropping a WhatsApp chat snapshot");
                return;
            }
        }
    }

    /// The sender's name from the contacts table, like the Go connector's
    /// `PersonName`. `None` for the owner's own messages ("Me").
    async fn sender_display_name(&self, client: &Client, info: &MessageInfo) -> Option<String> {
        if info.source.is_from_me || !is_person(&info.source.sender) {
            return None;
        }
        let person = self
            .titles
            .person(client, &info.source.sender, info.source.sender_alt.as_ref())
            .await;
        Some(person.name())
    }

    /// The display names of the people a message mentions, where known.
    async fn mentions(&self, client: &Client, message: &wa::Message) -> Vec<(String, String)> {
        let Some(context) = context_info(message) else {
            return Vec::new();
        };
        let mut mentions = Vec::new();
        for id in &context.mentioned_jid {
            let Ok(jid) = id.parse::<Jid>() else {
                continue;
            };
            let person = self.titles.person(client, &jid, None).await;
            if person.names.is_named() {
                mentions.push((jid.user.to_string(), person.name()));
            }
        }
        mentions
    }
}

/// A phone or LID JID: someone a name can be stored for.
fn is_person(jid: &Jid) -> bool {
    matches!(jid.server, Server::Pn | Server::Lid)
}

/// Replace each `@<user>` mention in `text` with `@<name>`. A mention only
/// matches a whole user part: `@1555` does not touch `@15550`.
pub fn render_mentions(text: &str, mentions: &[(String, String)]) -> String {
    if mentions.is_empty() {
        return text.to_owned();
    }
    let mut rendered = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        rendered.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let matched = mentions.iter().find(|(user, _)| {
            !user.is_empty()
                && after.starts_with(user.as_str())
                && !after[user.len()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric)
        });
        match matched {
            Some((user, name)) => {
                rendered.push('@');
                rendered.push_str(name);
                rest = &after[user.len()..];
            }
            None => {
                rendered.push('@');
                rest = after;
            }
        }
    }
    rendered.push_str(rest);
    rendered
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
/// `names` holds the sender's and the mentioned people's names.
fn normalize(
    info: &MessageInfo,
    message: &wa::Message,
    chat_title: Option<String>,
    names: &Names,
) -> Option<pb::ClientFrame> {
    let chat = canonical(&info.source.chat);
    // Ephemeral, view-once, device-sent and document-with-caption wrappers are
    // looked through: in a chat with disappearing messages on, nearly every
    // message arrives wrapped.
    let base = message.get_base_message();

    if let Some(protocol) = base.protocol_message.as_option() {
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
    if let Some(reaction) = base.reaction_message.as_option() {
        let target = reaction.key.as_option()?.id.clone()?;
        if target.is_empty() {
            return None;
        }
        return Some(frame(pb::client_frame::Body::Reaction(pb::ReactionEvent {
            chat_id: chat,
            thread_id: None,
            platform_message_id: target,
            sender: canonical(&info.source.sender),
            sender_name: sender_name(info, names.sender.as_deref()),
            emoji: reaction.text.clone().unwrap_or_default(),
            from_me: info.source.is_from_me,
        })));
    }

    let Some(text) = message.text_content() else {
        // Media messages are not bridged yet; dropping is deliberate rather
        // than delivering a caption that silently loses the attachment.
        if has_attachment(base) {
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
            text: Some(render_mentions(text, &names.mentions)),
            attachments: vec![],
            reply_to_platform_message_id: reply_target(message),
            timestamp_ms: info.timestamp.timestamp_millis(),
            sender_name: sender_name(info, names.sender.as_deref()),
            disappearing: message_timer(info, message),
        },
    )))
}

/// The per-message sender label: the name the contacts table resolved
/// (`resolved`), else the raw push name. The account owner's own messages are
/// labelled "Me" so a direct chat's two sides stay distinguishable on the
/// mirror; any sender whose device is not the primary phone is tagged as
/// writing from a linked device.
fn sender_name(info: &MessageInfo, resolved: Option<&str>) -> Option<String> {
    let name = if info.source.is_from_me {
        "Me".to_owned()
    } else {
        resolved
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| info.push_name.trim())
            .to_owned()
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

/// The reply, mention and timer context of a message's content, read off the
/// base message so ephemeral, view-once and device-sent wrappers are looked
/// through.
pub fn context_info(message: &wa::Message) -> Option<&wa::ContextInfo> {
    let base = message.get_base_message();
    base.extended_text_message
        .as_option()
        .and_then(|m| m.context_info.as_option())
        .or_else(|| {
            base.image_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.video_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.document_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.audio_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.sticker_message
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
        let event = create(normalize(
            &info(Some(86_400)),
            &timed_text(),
            None,
            &Names::default(),
        ));
        assert_eq!(
            event.disappearing,
            Some(pb::DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: Some(1_700_000_000),
            })
        );
        for none in [Some(0), None] {
            let event = create(normalize(
                &info(none),
                &timed_text(),
                None,
                &Names::default(),
            ));
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
            &Names::default(),
        ));
        assert_eq!(titled.chat_title.as_deref(), Some("[P] Alice (1555)"));
        let untitled = create(normalize(&info(None), &message, None, &Names::default()));
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
                &Names::default(),
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
            &Names::default(),
        ));
        assert_eq!(
            event.updates[0].disappearing,
            Some(pb::DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: Some(1_700_000_500),
            })
        );
    }

    #[test]
    fn mentions_render_as_names() {
        let mentions = vec![
            ("1555".to_owned(), "Alice Smith".to_owned()),
            ("8888".to_owned(), "Bob (1666)".to_owned()),
        ];
        assert_eq!(
            render_mentions("hi @1555 and @8888, see @1999", &mentions),
            "hi @Alice Smith and @Bob (1666), see @1999"
        );
        // Only a whole user part matches.
        assert_eq!(
            render_mentions("@15550 @1555", &mentions),
            "@15550 @Alice Smith"
        );
        assert_eq!(render_mentions("mail me@ x @", &mentions), "mail me@ x @");
        assert_eq!(render_mentions("@1555", &[]), "@1555");
        assert_eq!(
            render_mentions("ünï @1555!", &mentions),
            "ünï @Alice Smith!"
        );
    }

    #[test]
    fn a_create_carries_the_resolved_names() {
        let mut info = info(None);
        info.source.chat = "123@g.us".parse().unwrap();
        let message = wa::Message::text_with_context(
            "ping @1777",
            wa::ContextInfo {
                mentioned_jid: vec!["1777@s.whatsapp.net".into()],
                ..Default::default()
            },
        );
        let names = Names {
            sender: Some("Alice Smith".into()),
            mentions: vec![("1777".into(), "Bob (1777)".into())],
        };
        let event = create(normalize(&info, &message, None, &names));
        assert_eq!(event.sender_name.as_deref(), Some("Alice Smith"));
        assert_eq!(event.text.as_deref(), Some("ping @Bob (1777)"));
        // Without a resolved name, the push name; the owner is "Me".
        let event = create(normalize(&info, &message, None, &Names::default()));
        assert_eq!(event.sender_name.as_deref(), Some("Alice"));
        info.source.is_from_me = true;
        let event = create(normalize(&info, &message, None, &names));
        assert_eq!(event.sender_name.as_deref(), Some("Me"));
    }

    /// `inner` inside a disappearing-messages wrapper, as WhatsApp sends nearly
    /// every message in a chat with a timer on.
    fn ephemeral(inner: wa::Message) -> wa::Message {
        wa::Message {
            ephemeral_message: MessageField::some(wa::message::FutureProofMessage {
                message: MessageField::some(inner),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn key(id: &str) -> MessageField<wa::MessageKey> {
        MessageField::some(wa::MessageKey {
            id: Some(id.into()),
            ..Default::default()
        })
    }

    fn protocol(
        kind: wa::message::protocol_message::Type,
        edited: Option<wa::Message>,
    ) -> wa::Message {
        wa::Message {
            protocol_message: MessageField::some(wa::message::ProtocolMessage {
                r#type: Some(kind),
                key: key("wa-0"),
                edited_message: edited.map(MessageField::some).unwrap_or_default(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_wrapped_reaction_is_bridged() {
        let message = ephemeral(wa::Message {
            reaction_message: MessageField::some(wa::message::ReactionMessage {
                key: key("wa-0"),
                text: Some("👍".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        match normalize(&info(None), &message, None, &Names::default()).and_then(|f| f.body) {
            Some(pb::client_frame::Body::Reaction(event)) => {
                assert_eq!(event.platform_message_id, "wa-0");
                assert_eq!(event.emoji, "👍");
            }
            other => panic!("expected a reaction, got {other:?}"),
        }
    }

    #[test]
    fn a_wrapped_edit_is_bridged() {
        let message = ephemeral(protocol(
            wa::message::protocol_message::Type::MESSAGE_EDIT,
            Some(wa::Message::text("new")),
        ));
        match normalize(&info(None), &message, None, &Names::default()).and_then(|f| f.body) {
            Some(pb::client_frame::Body::MessageEdit(event)) => {
                assert_eq!(event.platform_message_id, "wa-0");
                assert_eq!(event.text.as_deref(), Some("new"));
            }
            other => panic!("expected an edit, got {other:?}"),
        }
    }

    #[test]
    fn a_wrapped_revoke_is_bridged() {
        let message = ephemeral(protocol(wa::message::protocol_message::Type::REVOKE, None));
        match normalize(&info(None), &message, None, &Names::default()).and_then(|f| f.body) {
            Some(pb::client_frame::Body::MessageDelete(event)) => {
                assert_eq!(event.platform_message_id, "wa-0");
            }
            other => panic!("expected a delete, got {other:?}"),
        }
    }

    #[test]
    fn a_wrapped_reply_keeps_its_parent() {
        let message = ephemeral(wa::Message::text_with_context(
            "sure",
            wa::ContextInfo {
                stanza_id: Some("wa-0".into()),
                ..Default::default()
            },
        ));
        let event = create(normalize(&info(None), &message, None, &Names::default()));
        assert_eq!(event.reply_to_platform_message_id.as_deref(), Some("wa-0"));
    }

    #[test]
    fn a_wrapped_message_carries_its_timer_timestamp() {
        let event = create(normalize(
            &info(Some(86_400)),
            &ephemeral(timed_text()),
            None,
            &Names::default(),
        ));
        assert_eq!(
            event.disappearing,
            Some(pb::DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: Some(1_700_000_000),
            })
        );
    }
}
