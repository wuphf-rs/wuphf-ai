//! Outbound dispatch execution: the core's message commands become WhatsApp
//! sends. WhatsApp has no threads, so dispatches never carry a thread target
//! and no delivery ever reports a provisioned thread.
//!
//! The sender reaches WhatsApp through [`WaClient`] and the core's media store
//! through [`CoreMedia`], so everything it decides — each part's kind and
//! options, caption, reply context, timer, order and failure class — is
//! tested against fakes; the live clients are thin adapters.

use std::future::Future;

use whatsapp_rust::Jid;
use whatsapp_rust::wacore::proto_helpers::{
    MessageBuilderExt, MessageExt, build_quote_context_with_info,
};
use whatsapp_rust::waproto::whatsapp as wa;
use wuphf_connector::{Delivery, Failure, MediaClient, Sender, SessionError};
use wuphf_proto::connector as pb;

use crate::chat_state::{mute_call, now_ms};
use crate::config::Runtime;
use crate::disappearing::{TimerFallback, stamp};
use crate::outbound::{outbound_media, quoted_body, text_exceeds_limit, text_file_media};
use crate::wa_client::{ChatAction, LiveClient, WaClient};

/// The core's media store, as the sender needs it: the bytes a dispatch's
/// attachment names.
pub trait CoreMedia: Send + Sync + 'static {
    fn download(
        &self,
        media_id: &str,
    ) -> impl Future<Output = Result<Vec<u8>, SessionError>> + Send;
}

impl CoreMedia for MediaClient {
    async fn download(&self, media_id: &str) -> Result<Vec<u8>, SessionError> {
        MediaClient::download(self, media_id).await
    }
}

pub struct WhatsAppSender<C = LiveClient, M = MediaClient> {
    client: C,
    media: M,
    /// How copies are rendered (`[rendering]`), read once at startup.
    runtime: Runtime,
    /// Set when the connector stamps disappearing timers: what to do with a
    /// dispatch that arrives without one.
    fallback: Option<TimerFallback>,
}

impl<C: WaClient, M: CoreMedia> WhatsAppSender<C, M> {
    pub fn new(client: C, media: M, runtime: Runtime, fallback: Option<TimerFallback>) -> Self {
        Self {
            client,
            media,
            runtime,
            fallback,
        }
    }

    /// The reply context of a dispatch: none for a system note (the bridge
    /// speaking quotes nothing) or a plain message, the quote otherwise. A
    /// reply id without its context can never be quoted, so it is permanent.
    fn reply_context(
        &self,
        dispatch: &pb::MessageCreateDispatch,
        chat: &Jid,
    ) -> Result<Option<wa::ContextInfo>, Failure> {
        if dispatch.system_note {
            return Ok(None);
        }
        match &dispatch.reply_context {
            Some(reply) => Ok(Some(self.quote_context(chat, reply)?)),
            None if dispatch.reply_to_platform_message_id.is_some() => Err(Failure::permanent(
                "outbound WhatsApp reply requires quoted-message context",
            )),
            None => Ok(None),
        }
    }

    /// The quote metadata for a reply. The quoted copy at this endpoint is
    /// always bridge-authored, since the router never dispatches a message
    /// back to its origin endpoint, so the quoted participant is our own
    /// account. The quoted body is a typed stub of the parent's media, so the
    /// preview shows a photo rather than empty text.
    fn quote_context(
        &self,
        chat: &Jid,
        reply: &pb::ReplyContext,
    ) -> Result<wa::ContextInfo, Failure> {
        let own = self.client.own_pn()?.ok_or_else(|| {
            Failure::permanent("outbound WhatsApp reply requires a paired account JID")
        })?;
        Ok(build_quote_context_with_info(
            reply.platform_message_id.clone(),
            &own.to_non_ad(),
            chat,
            chat,
            &quoted_body(reply),
        ))
    }

    /// The message for one attachment: downloaded from the core (a failure is
    /// transient: the core redelivers), then uploaded to WhatsApp.
    async fn attachment_message(
        &self,
        attachment: &pb::Attachment,
        caption: &str,
    ) -> Result<wa::Message, Failure> {
        let media = outbound_media(
            attachment,
            caption,
            self.runtime.rendering.attachments_as_files,
        )?;
        let bytes = self
            .media
            .download(&attachment.media_id)
            .await
            .map_err(|error| {
                Failure::Transient(
                    anyhow::Error::new(error).context("download media from the core"),
                )
            })?;
        self.client.upload(bytes, media).await
    }

    /// A dispatch with several attachments, as consecutive messages: WhatsApp
    /// carries one medium per message. The first carries the text as its
    /// caption and the reply context; every one carries the timer; the first
    /// one's id is the copy. A failure after the first was sent is permanent:
    /// the sent parts cannot be recalled, so a redelivery would duplicate them.
    async fn send_attachments(
        &self,
        chat: Jid,
        dispatch: &pb::MessageCreateDispatch,
        context: Option<wa::ContextInfo>,
        timer: Option<&pb::DisappearingTimer>,
    ) -> Result<String, Failure> {
        let text = dispatch.text.as_deref().unwrap_or_default();
        let mut context = context;
        let mut first = None;
        for (index, attachment) in dispatch.attachments.iter().enumerate() {
            let caption = if index == 0 { text } else { "" };
            let part = async {
                let mut message = self.attachment_message(attachment, caption).await?;
                if let Some(context) = context.take() {
                    message.set_context_info(context);
                }
                stamp(&mut message, timer);
                self.client.send(chat.clone(), message).await
            };
            let id = part.await.map_err(|error| group_failure(error, index))?;
            first.get_or_insert(id);
        }
        Ok(first.unwrap_or_default())
    }
}

/// A failure that leaves a multi-attachment message partly sent is permanent,
/// so the dispatch is dead-lettered rather than redelivered into duplicates;
/// one on the first part has sent nothing and keeps its own class.
fn group_failure(error: Failure, index: usize) -> Failure {
    if index == 0 {
        return error;
    }
    Failure::permanent(format!(
        "attachment {} of a multi-attachment message failed after {index} were sent: {error}",
        index + 1
    ))
}

/// A target chat that does not parse can never succeed on retry, so it is
/// permanent and the core dead-letters the dispatch.
fn parse_chat(chat_id: &str) -> Result<Jid, Failure> {
    chat_id.parse().map_err(|error| {
        Failure::permanent(format!("parse WhatsApp target chat {chat_id:?}: {error}"))
    })
}

impl<C: WaClient, M: CoreMedia> Sender for WhatsAppSender<C, M> {
    async fn send_message(&self, dispatch: pb::MessageCreateDispatch) -> Result<Delivery, Failure> {
        let chat = parse_chat(&dispatch.target_chat_id)?;
        let context = self.reply_context(&dispatch, &chat)?;
        // Every part of a content send carries the chat's timer, so it
        // disappears like the rest of the chat instead of showing "will not
        // disappear".
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
        let message_id = if dispatch.attachments.len() > 1 {
            self.send_attachments(chat, &dispatch, context, timer.as_ref())
                .await?
        } else {
            let text = dispatch.text.as_deref().unwrap_or_default();
            let mut message = match dispatch.attachments.first() {
                Some(attachment) => self.attachment_message(attachment, text).await?,
                None if text_exceeds_limit(text, self.runtime.rendering.max_message_length) => {
                    self.client
                        .upload(text.as_bytes().to_vec(), text_file_media())
                        .await?
                }
                None => wa::Message::text(text),
            };
            if let Some(context) = context {
                message.set_context_info(context);
            }
            stamp(&mut message, timer.as_ref());
            self.client.send(chat, message).await?
        };
        Ok(Delivery {
            message_id,
            thread_id: None,
        })
    }

    /// WhatsApp keeps the original message id across an edit, so the id the
    /// edit API returns is the target id itself.
    async fn edit_message(&self, dispatch: pb::MessageEditDispatch) -> Result<String, Failure> {
        let chat = parse_chat(&dispatch.target_chat_id)?;
        let text = dispatch.text.unwrap_or_default();
        self.client
            .edit(
                chat,
                dispatch.target_platform_message_id.clone(),
                wa::Message::text(text),
            )
            .await
    }

    /// Bring the chat to the dispatched pin / mute / archive state, one chat
    /// action per field present. Emits nothing: WhatsApp's own app-state echo
    /// comes back through the event handlers, and the core finds it equal to
    /// what it stored. WhatsApp has no threads, so `target_thread_id` is
    /// never set here.
    async fn apply_chat_state(&self, dispatch: pb::ChatStateDispatch) -> Result<String, Failure> {
        let chat = parse_chat(&dispatch.target_chat_id)?;
        if let Some(pinned) = dispatch.pinned {
            self.client
                .chat_action(chat.clone(), ChatAction::Pin(pinned))
                .await?;
        }
        if let Some(mute) = dispatch.mute {
            let call = mute_call(mute.muted_until, now_ms());
            self.client
                .chat_action(chat.clone(), ChatAction::Mute(call))
                .await?;
        }
        if let Some(archived) = dispatch.archived {
            self.client
                .chat_action(chat, ChatAction::Archive(archived))
                .await?;
        }
        Ok(dispatch.target_chat_id)
    }

    /// Revoking our own (bridge-authored) copy; the target id is returned
    /// unchanged for the acknowledgement.
    async fn delete_message(&self, dispatch: pb::MessageDeleteDispatch) -> Result<String, Failure> {
        let chat = parse_chat(&dispatch.target_chat_id)?;
        self.client
            .revoke(chat, dispatch.target_platform_message_id.clone())
            .await?;
        Ok(dispatch.target_platform_message_id)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use tokio::sync::mpsc;
    use whatsapp_rust::waproto::buffa::MessageField;

    use super::*;
    use crate::chat_state::MuteCall;
    use crate::config::Rendering;
    use crate::groups::GroupFacts;
    use crate::normalize::context_info;
    use crate::wa_client::OutboundMedia;

    const DM: &str = "1555@s.whatsapp.net";
    const GROUP: &str = "123@g.us";

    /// WhatsApp, recorded: every send, upload, edit, revoke and chat action.
    #[derive(Default)]
    struct FakeClient {
        sends: Mutex<Vec<(Jid, wa::Message)>>,
        uploads: Mutex<Vec<(Vec<u8>, OutboundMedia)>>,
        edits: Mutex<Vec<(Jid, String, wa::Message)>>,
        revokes: Mutex<Vec<(Jid, String)>>,
        actions: Mutex<Vec<ChatAction>>,
        /// The send (0-based) that fails, transiently.
        fail_send_at: Option<usize>,
        groups: HashMap<String, GroupFacts>,
    }

    impl WaClient for FakeClient {
        async fn send(&self, to: Jid, message: wa::Message) -> Result<String, Failure> {
            let mut sends = self.sends.lock().unwrap();
            if self.fail_send_at == Some(sends.len()) {
                return Err(Failure::Transient(anyhow::anyhow!("network down")));
            }
            sends.push((to, message));
            Ok(format!("wa-{}", sends.len()))
        }

        async fn edit(&self, to: Jid, id: String, message: wa::Message) -> Result<String, Failure> {
            self.edits.lock().unwrap().push((to, id.clone(), message));
            Ok(id)
        }

        async fn revoke(&self, to: Jid, id: String) -> Result<(), Failure> {
            self.revokes.lock().unwrap().push((to, id));
            Ok(())
        }

        /// A message whose body matches the request's kind and carries its
        /// options, so the sender's context and timer land on a real body.
        async fn upload(
            &self,
            bytes: Vec<u8>,
            media: OutboundMedia,
        ) -> Result<wa::Message, Failure> {
            self.uploads.lock().unwrap().push((bytes, media.clone()));
            Ok(match media {
                OutboundMedia::Image(options) => wa::Message {
                    image_message: MessageField::some(wa::message::ImageMessage {
                        caption: options.caption,
                        mimetype: options.mimetype,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                OutboundMedia::Video(options) => wa::Message {
                    video_message: MessageField::some(wa::message::VideoMessage {
                        caption: options.caption,
                        mimetype: options.mimetype,
                        gif_playback: options.gif_playback,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                OutboundMedia::Audio(options) => wa::Message {
                    audio_message: MessageField::some(wa::message::AudioMessage {
                        mimetype: options.mimetype,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                OutboundMedia::Document(options) => wa::Message {
                    document_message: MessageField::some(wa::message::DocumentMessage {
                        caption: options.caption,
                        mimetype: options.mimetype,
                        file_name: options.file_name,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            })
        }

        async fn chat_action(&self, _chat: Jid, action: ChatAction) -> Result<(), Failure> {
            self.actions.lock().unwrap().push(action);
            Ok(())
        }

        fn own_pn(&self) -> Result<Option<Jid>, Failure> {
            Ok(Some("1999@s.whatsapp.net".parse().unwrap()))
        }

        async fn group_facts(&self, group: &Jid) -> Option<GroupFacts> {
            self.groups.get(&group.to_string()).cloned()
        }
    }

    /// The core's media store.
    #[derive(Default)]
    struct FakeMedia {
        blobs: HashMap<String, Vec<u8>>,
        fail: bool,
    }

    impl CoreMedia for FakeMedia {
        async fn download(&self, media_id: &str) -> Result<Vec<u8>, SessionError> {
            if self.fail {
                return Err(SessionError::StreamClosed);
            }
            Ok(self.blobs.get(media_id).cloned().unwrap_or_default())
        }
    }

    fn media(ids: &[&str]) -> FakeMedia {
        FakeMedia {
            blobs: ids
                .iter()
                .map(|id| ((*id).to_owned(), id.as_bytes().to_vec()))
                .collect(),
            fail: false,
        }
    }

    fn sender(client: FakeClient, media: FakeMedia) -> WhatsAppSender<FakeClient, FakeMedia> {
        WhatsAppSender::new(client, media, Runtime::default(), None)
    }

    fn rendering(attachments_as_files: bool, max_message_length: u32) -> Runtime {
        Runtime {
            rendering: Rendering {
                attachments_as_files,
                max_message_length,
            },
        }
    }

    fn text(body: &str) -> pb::MessageCreateDispatch {
        pb::MessageCreateDispatch {
            target_chat_id: DM.into(),
            text: Some(body.into()),
            ..Default::default()
        }
    }

    fn image(id: &str) -> pb::Attachment {
        pb::Attachment {
            media_id: id.into(),
            kind: pb::attachment::Kind::Image.into(),
            mime_type: Some("image/png".into()),
            ..Default::default()
        }
    }

    fn reply_to(id: &str) -> pb::ReplyContext {
        pb::ReplyContext {
            platform_message_id: id.into(),
            text: Some("earlier".into()),
            ..Default::default()
        }
    }

    fn timer() -> pb::DisappearingTimer {
        pb::DisappearingTimer {
            seconds: 86_400,
            setting_timestamp: Some(1_700_000_000),
        }
    }

    fn sends(sender: &WhatsAppSender<FakeClient, FakeMedia>) -> Vec<wa::Message> {
        let sends = sender.client.sends.lock().unwrap();
        sends.iter().map(|(_, message)| message.clone()).collect()
    }

    fn context(message: &wa::Message) -> &wa::ContextInfo {
        context_info(message).expect("the message carries a context")
    }

    #[tokio::test]
    async fn text_is_sent_as_text() {
        let sender = sender(FakeClient::default(), FakeMedia::default());
        let delivery = sender.send_message(text("hi")).await.unwrap();
        assert_eq!(delivery.message_id, "wa-1");
        assert_eq!(delivery.thread_id, None);
        assert_eq!(sends(&sender)[0].text_content(), Some("hi"));
    }

    #[tokio::test]
    async fn a_reply_quotes_its_parent() {
        let sender = sender(FakeClient::default(), FakeMedia::default());
        let dispatch = pb::MessageCreateDispatch {
            reply_context: Some(reply_to("wa-0")),
            ..text("sure")
        };
        sender.send_message(dispatch).await.unwrap();
        let message = &sends(&sender)[0];
        assert_eq!(message.text_content(), Some("sure"));
        let quote = context(message);
        assert_eq!(quote.stanza_id.as_deref(), Some("wa-0"));
        assert_eq!(quote.participant.as_deref(), Some("1999@s.whatsapp.net"));
        assert_eq!(
            quote.quoted_message.conversation.as_deref(),
            Some("earlier")
        );
    }

    #[tokio::test]
    async fn a_reply_id_without_a_context_is_permanent() {
        let sender = sender(FakeClient::default(), FakeMedia::default());
        let dispatch = pb::MessageCreateDispatch {
            reply_to_platform_message_id: Some("wa-0".into()),
            ..text("sure")
        };
        let Err(failure) = sender.send_message(dispatch).await else {
            panic!("a reply without its context is refused");
        };
        assert!(failure.is_permanent());
    }

    #[tokio::test]
    async fn a_note_never_quotes_and_sends_its_photo() {
        let sender = sender(FakeClient::default(), media(&["m-1"]));
        let dispatch = pb::MessageCreateDispatch {
            system_note: true,
            reply_context: Some(reply_to("wa-0")),
            attachments: vec![image("m-1")],
            ..text("🖼️ Photo changed")
        };
        sender.send_message(dispatch).await.unwrap();
        let message = &sends(&sender)[0];
        assert_eq!(
            message.image_message.caption.as_deref(),
            Some("🖼️ Photo changed")
        );
        assert!(context_info(message).is_none());
    }

    #[tokio::test]
    async fn the_timer_is_stamped_on_text_and_an_off_one_sets_nothing() {
        let sender = sender(FakeClient::default(), FakeMedia::default());
        let dispatch = pb::MessageCreateDispatch {
            disappearing: Some(timer()),
            ..text("hi")
        };
        sender.send_message(dispatch).await.unwrap();
        let stamped = context(&sends(&sender)[0]).clone();
        assert_eq!(stamped.expiration, Some(86_400));
        assert_eq!(stamped.ephemeral_setting_timestamp, Some(1_700_000_000));

        for disappearing in [Some(pb::DisappearingTimer::default()), None] {
            let dispatch = pb::MessageCreateDispatch {
                disappearing,
                ..text("hi")
            };
            sender.send_message(dispatch).await.unwrap();
        }
        for message in &sends(&sender)[1..] {
            assert!(context_info(message).is_none_or(|context| context.expiration.is_none()));
        }
    }

    #[tokio::test]
    async fn edits_deletes_and_chat_state_return_the_target_id() {
        let sender = sender(FakeClient::default(), FakeMedia::default());
        let edit = pb::MessageEditDispatch {
            target_chat_id: DM.into(),
            target_platform_message_id: "wa-7".into(),
            text: Some("new".into()),
            ..Default::default()
        };
        assert_eq!(sender.edit_message(edit).await.unwrap(), "wa-7");
        let delete = pb::MessageDeleteDispatch {
            target_chat_id: DM.into(),
            target_platform_message_id: "wa-7".into(),
            ..Default::default()
        };
        assert_eq!(sender.delete_message(delete).await.unwrap(), "wa-7");
        let edited = sender.client.edits.lock().unwrap()[0].2.clone();
        assert_eq!(edited.text_content(), Some("new"));
        assert!(context_info(&edited).is_none());
        assert_eq!(sender.client.revokes.lock().unwrap()[0].1, "wa-7");

        let state = pb::ChatStateDispatch {
            target_chat_id: DM.into(),
            pinned: Some(true),
            mute: Some(pb::MuteState { muted_until: -1 }),
            archived: Some(false),
            ..Default::default()
        };
        assert_eq!(sender.apply_chat_state(state).await.unwrap(), DM);
        assert_eq!(
            *sender.client.actions.lock().unwrap(),
            [
                ChatAction::Pin(true),
                ChatAction::Mute(MuteCall::Forever),
                ChatAction::Archive(false),
            ]
        );
    }

    #[tokio::test]
    async fn a_group_without_a_dispatched_timer_uses_its_own() {
        let client = FakeClient {
            groups: HashMap::from([(
                GROUP.to_owned(),
                GroupFacts {
                    disappearing_seconds: 604_800,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let (inputs, mut frames) = mpsc::channel(4);
        let sender = WhatsAppSender::new(
            client,
            FakeMedia::default(),
            Runtime::default(),
            Some(TimerFallback::new(inputs)),
        );
        let dispatch = pb::MessageCreateDispatch {
            target_chat_id: GROUP.into(),
            ..text("hi")
        };
        sender.send_message(dispatch).await.unwrap();
        assert_eq!(context(&sends(&sender)[0]).expiration, Some(604_800));
        assert!(matches!(
            frames.try_recv().unwrap().body,
            Some(pb::client_frame::Body::ChatUpdate(_))
        ));
    }

    #[tokio::test]
    async fn an_attachment_is_downloaded_and_uploaded_with_its_caption() {
        let sender = sender(FakeClient::default(), media(&["m-1"]));
        let dispatch = pb::MessageCreateDispatch {
            attachments: vec![image("m-1")],
            ..text("look")
        };
        sender.send_message(dispatch).await.unwrap();
        let (bytes, upload) = sender.client.uploads.lock().unwrap()[0].clone();
        assert_eq!(bytes, b"m-1");
        let OutboundMedia::Image(options) = upload else {
            panic!("an image upload");
        };
        assert_eq!(options.caption.as_deref(), Some("look"));
        assert_eq!(
            sends(&sender)[0].image_message.caption.as_deref(),
            Some("look")
        );
    }

    #[tokio::test]
    async fn attachments_as_files_sends_a_document() {
        let sender = WhatsAppSender::new(
            FakeClient::default(),
            media(&["m-1"]),
            rendering(true, 0),
            None,
        );
        let dispatch = pb::MessageCreateDispatch {
            attachments: vec![image("m-1")],
            ..text("look")
        };
        sender.send_message(dispatch).await.unwrap();
        let message = &sends(&sender)[0];
        assert!(message.document_message.is_set());
        assert_eq!(message.document_message.caption.as_deref(), Some("look"));
    }

    #[tokio::test]
    async fn over_length_text_goes_out_as_a_file_and_short_text_stays_inline() {
        let sender = WhatsAppSender::new(
            FakeClient::default(),
            FakeMedia::default(),
            rendering(false, 5),
            None,
        );
        let dispatch = pb::MessageCreateDispatch {
            reply_context: Some(reply_to("wa-0")),
            ..text("héllo!")
        };
        sender.send_message(dispatch).await.unwrap();
        sender.send_message(text("héllo")).await.unwrap();
        let (bytes, upload) = sender.client.uploads.lock().unwrap()[0].clone();
        assert_eq!(bytes, "héllo!".as_bytes());
        let OutboundMedia::Document(options) = upload else {
            panic!("a document upload");
        };
        assert_eq!(options.file_name.as_deref(), Some("message.txt"));
        assert_eq!(options.mimetype.as_deref(), Some("text/plain"));
        let sent = sends(&sender);
        assert_eq!(context(&sent[0]).stanza_id.as_deref(), Some("wa-0"));
        assert_eq!(sent[1].text_content(), Some("héllo"));
        assert_eq!(sender.client.uploads.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_reply_context_and_the_timer_land_on_the_media() {
        let sender = sender(FakeClient::default(), media(&["m-1"]));
        let dispatch = pb::MessageCreateDispatch {
            reply_context: Some(pb::ReplyContext {
                attachments: vec![image("m-0")],
                ..reply_to("wa-0")
            }),
            attachments: vec![image("m-1")],
            disappearing: Some(timer()),
            ..text("look")
        };
        sender.send_message(dispatch).await.unwrap();
        let message = &sends(&sender)[0];
        let quote = context(message);
        assert_eq!(quote.stanza_id.as_deref(), Some("wa-0"));
        assert_eq!(quote.expiration, Some(86_400));
        // The parent is quoted as the photo it was.
        assert!(quote.quoted_message.image_message.is_set());
    }

    #[tokio::test]
    async fn a_failed_core_download_is_transient() {
        let sender = sender(
            FakeClient::default(),
            FakeMedia {
                fail: true,
                ..Default::default()
            },
        );
        let dispatch = pb::MessageCreateDispatch {
            attachments: vec![image("m-1")],
            ..text("look")
        };
        let Err(failure) = sender.send_message(dispatch).await else {
            panic!("the download failed");
        };
        assert!(!failure.is_permanent());
        assert!(sends(&sender).is_empty());
    }

    fn album() -> pb::MessageCreateDispatch {
        pb::MessageCreateDispatch {
            reply_context: Some(reply_to("wa-0")),
            attachments: vec![image("m-1"), image("m-2"), image("m-3")],
            disappearing: Some(timer()),
            ..text("three")
        }
    }

    #[tokio::test]
    async fn several_attachments_caption_and_quote_only_the_first_and_time_every_part() {
        let sender = sender(FakeClient::default(), media(&["m-1", "m-2", "m-3"]));
        let delivery = sender.send_message(album()).await.unwrap();
        assert_eq!(delivery.message_id, "wa-1");
        let sent = sends(&sender);
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0].image_message.caption.as_deref(), Some("three"));
        assert_eq!(sent[1].image_message.caption, None);
        assert_eq!(context(&sent[0]).stanza_id.as_deref(), Some("wa-0"));
        for part in &sent {
            assert_eq!(context(part).expiration, Some(86_400));
        }
        assert_eq!(context(&sent[1]).stanza_id, None);
    }

    #[tokio::test]
    async fn a_failure_after_the_first_part_is_permanent() {
        let client = FakeClient {
            fail_send_at: Some(1),
            ..Default::default()
        };
        let sender = sender(client, media(&["m-1", "m-2", "m-3"]));
        let Err(failure) = sender.send_message(album()).await else {
            panic!("the second part failed");
        };
        assert!(failure.is_permanent());
        assert!(
            failure
                .to_string()
                .contains("attachment 2 of a multi-attachment message failed after 1 were sent"),
            "{failure}"
        );
    }

    #[tokio::test]
    async fn a_failure_on_the_first_part_stays_retryable() {
        let client = FakeClient {
            fail_send_at: Some(0),
            ..Default::default()
        };
        let sender = sender(client, media(&["m-1", "m-2", "m-3"]));
        let Err(failure) = sender.send_message(album()).await else {
            panic!("the first part failed");
        };
        assert!(!failure.is_permanent());
    }

    #[tokio::test]
    async fn the_timer_is_stamped_on_every_kind() {
        use pb::attachment::Kind;
        let kinds = [
            Kind::Image,
            Kind::Video,
            Kind::Audio,
            Kind::File,
            Kind::Sticker,
        ];
        let sender = WhatsAppSender::new(
            FakeClient::default(),
            media(&["m"]),
            rendering(false, 3),
            None,
        );
        for kind in kinds {
            let dispatch = pb::MessageCreateDispatch {
                attachments: vec![pb::Attachment {
                    media_id: "m".into(),
                    kind: kind.into(),
                    ..Default::default()
                }],
                disappearing: Some(timer()),
                ..text("")
            };
            sender.send_message(dispatch).await.unwrap();
        }
        let over_length = pb::MessageCreateDispatch {
            disappearing: Some(timer()),
            ..text("too long")
        };
        sender.send_message(over_length).await.unwrap();
        let sent = sends(&sender);
        assert_eq!(sent.len(), kinds.len() + 1);
        for message in &sent {
            assert_eq!(context(message).expiration, Some(86_400), "{message:?}");
        }
    }
}
