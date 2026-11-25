//! Outbound dispatch execution: the core's message commands become WhatsApp
//! sends. WhatsApp has no threads, so dispatches never carry a thread target
//! and no delivery ever reports a provisioned thread.
//!
//! The sender reaches WhatsApp through [`WaClient`] and the core's media store
//! through [`CoreMedia`], so everything it decides — each part's kind and
//! options, caption, reply context, timer, order and failure class — is
//! tested against fakes; the live clients are thin adapters.

use std::future::Future;
use std::sync::Arc;

use tracing::warn;
use whatsapp_rust::types::message::AddressingMode;
use whatsapp_rust::wacore::proto_helpers::{
    MessageBuilderExt, MessageExt, build_quote_context_with_info,
};
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Jid, Server};
use wuphf_connector::settings::Live;
use wuphf_connector::{Delivery, Failure, MediaClient, MediaConverter, Sender, SessionError};
use wuphf_proto::connector as pb;

use crate::addressing::AddressBook;
use crate::chat_state::{mute_call, now_ms};
use crate::config::Runtime;
use crate::disappearing::{TimerFallback, stamp};
use crate::identity::Identity;
use crate::outbound::{optional, outbound_media, quoted_body, text_exceeds_limit, text_file_media};
// The sender never posts to the story feed for a dispatch (plan 083): the core
// keeps it receive-only, and this is the backstop if it ever does not.
use crate::posting::is_status_broadcast;
use crate::stickermeta::apply_sticker_metadata;
use crate::wa_client::{ChatAction, LiveClient, OutboundMedia, StickerOptions, WaClient};

/// A WhatsApp sticker's format; an animated one is an animated WebP.
const WEBP: &str = "image/webp";

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

/// Converts stored media to another format: the core's media.convert
/// provider, in production (Go `StickerConverter`).
pub trait StickerConverter: Send + Sync + 'static {
    fn convert(
        &self,
        media_id: &str,
        source_mime: &str,
        source_name: &str,
        target_mime: &str,
    ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send;
}

impl StickerConverter for MediaConverter {
    async fn convert(
        &self,
        media_id: &str,
        source_mime: &str,
        source_name: &str,
        target_mime: &str,
    ) -> anyhow::Result<Vec<u8>> {
        Ok(MediaConverter::convert(self, media_id, source_mime, source_name, target_mime).await?)
    }
}

pub struct WhatsAppSender<C = LiveClient, M = MediaClient, S = MediaConverter> {
    client: C,
    media: M,
    converter: S,
    /// How copies are rendered (`[rendering]`) and stickers labelled: the
    /// live runtime settings, read once per dispatch (Go `pin`), so every part
    /// of one dispatch sees the same value and a change applies from the next.
    runtime: Arc<Live<Runtime>>,
    /// Set when the connector stamps disappearing timers: what to do with a
    /// dispatch that arrives without one.
    fallback: Option<TimerFallback>,
    /// Dispatches name canonical ids; these put an address back into the form
    /// the chat is addressed in before it goes on the wire.
    identity: Arc<Identity>,
    addresses: Arc<AddressBook>,
}

impl<C: WaClient, M: CoreMedia, S: StickerConverter> WhatsAppSender<C, M, S> {
    pub fn new(
        client: C,
        media: M,
        converter: S,
        runtime: Arc<Live<Runtime>>,
        fallback: Option<TimerFallback>,
        identity: Arc<Identity>,
        addresses: Arc<AddressBook>,
    ) -> Self {
        Self {
            client,
            media,
            converter,
            runtime,
            fallback,
            identity,
            addresses,
        }
    }

    /// A dispatch's target chat in the form WhatsApp addresses it in, with that
    /// form for the frame's other addresses (Go `target`). A target that does
    /// not parse can never succeed, so it is permanent.
    async fn target(&self, chat_id: &str) -> Result<(Jid, Option<AddressingMode>), Failure> {
        self.client.linked()?;
        let chat = parse_chat(chat_id)?;
        let mode = self.addresses.mode(&self.client, &chat).await;
        let chat = self
            .addresses
            .in_mode(&self.identity, &self.client, &chat, mode)
            .await;
        Ok((chat, mode))
    }

    /// The reply context of a dispatch: none for a system note (the bridge
    /// speaking quotes nothing) or a plain message, the quote otherwise. A
    /// reply id without its context can never be quoted, so it is permanent.
    async fn reply_context(
        &self,
        dispatch: &pb::MessageCreateDispatch,
        chat: &Jid,
        mode: Option<AddressingMode>,
    ) -> Result<Option<wa::ContextInfo>, Failure> {
        if dispatch.system_note {
            return Ok(None);
        }
        match &dispatch.reply_context {
            Some(reply) => {
                // A quote of another chat (plan 083: a group message quoted
                // in a DM, a story in its poster's DM) names that chat, and
                // its author in that chat's address form.
                let (quoted_chat, mode) = match reply.chat_id.as_deref().filter(|id| !id.is_empty())
                {
                    Some(chat_id) => self.target(chat_id).await?,
                    None => (chat.clone(), mode),
                };
                Ok(Some(build_quote_context_with_info(
                    reply.platform_message_id.clone(),
                    &self
                        .message_author(
                            reply.authored_by_bridge,
                            reply.participant.as_deref(),
                            &reply.sender,
                            mode,
                        )
                        .await?,
                    &quoted_chat,
                    chat,
                    // A typed stub of the parent's media, so the preview
                    // shows a photo rather than empty text.
                    &quoted_body(reply),
                )))
            }
            None if dispatch.reply_to_platform_message_id.is_some() => Err(Failure::permanent(
                "outbound WhatsApp reply requires quoted-message context",
            )),
            None => Ok(None),
        }
    }

    /// Who WhatsApp is told wrote a message a quote or a reaction names (Go
    /// `messageAuthor`): WhatsApp links a quote by its id and author together,
    /// and a reaction by its key's author, so the wrong author shows the
    /// wrong name and leaves it unlinked. A copy the bridge received here (a
    /// message someone in the chat wrote) names its author in this platform's
    /// canonical ids: the group participant, else the chat peer, put into the
    /// chat's form. A copy the bridge sent was written by our own account,
    /// which a LID-addressed chat knows by its LID.
    async fn message_author(
        &self,
        authored_by_bridge: bool,
        participant: Option<&str>,
        sender: &str,
        mode: Option<AddressingMode>,
    ) -> Result<Jid, Failure> {
        if !authored_by_bridge {
            let author = participant
                .filter(|author| !author.is_empty())
                .unwrap_or(sender);
            if let Ok(author) = author.parse::<Jid>() {
                return Ok(self
                    .addresses
                    .in_mode(&self.identity, &self.client, &author, mode)
                    .await);
            }
        }
        self.addresses.own(&self.client, mode).ok_or_else(|| {
            Failure::permanent("naming our own WhatsApp message requires a paired account JID")
        })
    }

    /// The key a reaction names its message by, shaped as WhatsApp's own
    /// (the library's `MessageContext::message_key`): our own message is
    /// `from_me`, and someone else's in a group carries its author.
    async fn reaction_key(
        &self,
        dispatch: &pb::ReactionDispatch,
        chat: &Jid,
        mode: Option<AddressingMode>,
    ) -> Result<wa::MessageKey, Failure> {
        let author = self
            .message_author(
                dispatch.target_authored_by_bridge,
                dispatch.target_participant.as_deref(),
                &dispatch.target_sender,
                mode,
            )
            .await?;
        let own = self.addresses.own(&self.client, mode);
        let from_me = own.is_some_and(|own| own.user == author.user);
        Ok(wa::MessageKey {
            remote_jid: Some(chat.to_string()),
            from_me: Some(from_me),
            id: Some(dispatch.target_platform_message_id.clone()),
            participant: (!from_me && chat.server == Server::Group).then(|| author.to_string()),
        })
    }

    /// The owner's reaction on someone's story, sent the way WhatsApp's own
    /// quick reactions are (Go `reactToStory`): an emoji message in the
    /// poster's DM, quoting the story. A send to status@broadcast would reach
    /// everyone who sees the owner's stories. A cleared reaction, or one on
    /// the owner's own story, sends nothing.
    async fn react_to_story(
        &self,
        dispatch: pb::ReactionDispatch,
        status: &Jid,
        mode: Option<AddressingMode>,
    ) -> Result<String, Failure> {
        let story_id = dispatch.target_platform_message_id;
        let poster = dispatch
            .target_participant
            .as_deref()
            .filter(|poster| !poster.is_empty())
            .unwrap_or(&dispatch.target_sender);
        if dispatch.emoji.is_empty() || dispatch.target_authored_by_bridge || poster.is_empty() {
            return Ok(story_id);
        }
        let poster: Jid = poster.parse().map_err(|error| {
            Failure::permanent(format!("parse story poster {poster:?}: {error}"))
        })?;
        if is_status_broadcast(&poster) {
            return Ok(story_id);
        }
        let poster = poster.to_non_ad();
        let (dm, _) = self.target(&poster.to_string()).await?;
        let quoted = self
            .addresses
            .in_mode(&self.identity, &self.client, &poster, mode)
            .await;
        let context = build_quote_context_with_info(
            story_id.clone(),
            &quoted,
            status,
            &dm,
            &wa::Message::text(""),
        );
        let mut message = wa::Message::text(&dispatch.emoji);
        message.set_context_info(context);
        self.client.send(dm, message).await?;
        Ok(story_id)
    }

    /// The message for one attachment: downloaded from the core (a failure is
    /// transient: the core redelivers), then uploaded to WhatsApp.
    async fn attachment_message(
        &self,
        attachment: &pb::Attachment,
        caption: &str,
        runtime: &Runtime,
    ) -> Result<wa::Message, Failure> {
        let as_file = runtime.rendering.attachments_as_files;
        let media = outbound_media(attachment, caption, as_file)?;
        let bytes = self
            .media
            .download(&attachment.media_id)
            .await
            .map_err(|error| {
                Failure::Transient(
                    anyhow::Error::new(error).context("download media from the core"),
                )
            })?;
        if attachment.kind() == pb::attachment::Kind::Sticker && !as_file {
            return self
                .sticker_message(attachment, bytes, media, runtime)
                .await;
        }
        self.client.upload(bytes, media).await
    }

    /// A sticker as a WhatsApp sticker (Go `stickerMessage`). WhatsApp
    /// stickers are WebP, so an animated sticker in another format (a Telegram
    /// video or Lottie sticker) is converted to animated WebP by the core's
    /// media.convert provider first; a WebP then gets the pack metadata. When
    /// the conversion fails (no provider, a timeout), the original goes out as
    /// `fallback`, a file, so the message is not lost.
    async fn sticker_message(
        &self,
        attachment: &pb::Attachment,
        bytes: Vec<u8>,
        fallback: OutboundMedia,
        runtime: &Runtime,
    ) -> Result<wa::Message, Failure> {
        let mut bytes = bytes;
        let mut mime = attachment.mime_type.clone().unwrap_or_default();
        if attachment.animated && mime != WEBP {
            let source_name = attachment.file_name.as_deref().unwrap_or_default();
            match self
                .converter
                .convert(&attachment.media_id, &mime, source_name, WEBP)
                .await
            {
                Ok(converted) => (bytes, mime) = (converted, WEBP.to_owned()),
                Err(error) => {
                    warn!(
                        error = format!("{error:#}"),
                        media_id = attachment.media_id,
                        mime,
                        "could not convert a sticker to WebP; sending it as a file"
                    );
                    return self.client.upload(bytes, fallback).await;
                }
            }
        }
        if mime == WEBP {
            bytes = apply_sticker_metadata(bytes, &runtime.sticker_metadata());
        }
        let options = StickerOptions {
            mimetype: optional(Some(&mime)),
            is_animated: attachment.animated,
        };
        self.client
            .upload(bytes, OutboundMedia::Sticker(options))
            .await
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
        runtime: &Runtime,
    ) -> Result<String, Failure> {
        let text = dispatch.text.as_deref().unwrap_or_default();
        let mut context = context;
        let mut first = None;
        for (index, attachment) in dispatch.attachments.iter().enumerate() {
            let caption = if index == 0 { text } else { "" };
            let part = async {
                let mut message = self
                    .attachment_message(attachment, caption, runtime)
                    .await?;
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

/// The permanent failure for a dispatch that would post to status@broadcast.
fn refuse_status_post(operation: &str) -> Failure {
    Failure::permanent(format!(
        "refusing to {operation} status@broadcast: that would post a story to your contacts"
    ))
}

/// A target chat that does not parse can never succeed on retry, so it is
/// permanent and the core dead-letters the dispatch.
fn parse_chat(chat_id: &str) -> Result<Jid, Failure> {
    chat_id.parse().map_err(|error| {
        Failure::permanent(format!("parse WhatsApp target chat {chat_id:?}: {error}"))
    })
}

impl<C: WaClient, M: CoreMedia, S: StickerConverter> Sender for WhatsAppSender<C, M, S> {
    async fn send_message(&self, dispatch: pb::MessageCreateDispatch) -> Result<Delivery, Failure> {
        // One value for the whole dispatch (Go `pin`).
        let runtime = self.runtime.load();
        let (chat, mode) = self.target(&dispatch.target_chat_id).await?;
        if is_status_broadcast(&chat) {
            return Err(refuse_status_post("send to"));
        }
        let context = self.reply_context(&dispatch, &chat, mode).await?;
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
            self.send_attachments(chat, &dispatch, context, timer.as_ref(), &runtime)
                .await?
        } else {
            let text = dispatch.text.as_deref().unwrap_or_default();
            let mut message = match dispatch.attachments.first() {
                Some(attachment) => self.attachment_message(attachment, text, &runtime).await?,
                None if text_exceeds_limit(text, runtime.rendering.max_message_length) => {
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
        let (chat, _) = self.target(&dispatch.target_chat_id).await?;
        if is_status_broadcast(&chat) {
            return Err(refuse_status_post("edit on"));
        }
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
        let (chat, _) = self.target(&dispatch.target_chat_id).await?;
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

    /// The owner's reaction, set as this account's (the owner's); anyone
    /// else's is not the owner's to make, so it is acknowledged without
    /// sending (plan 080). An empty emoji removes the reaction. The target id
    /// is returned unchanged.
    async fn apply_reaction(&self, dispatch: pb::ReactionDispatch) -> Result<String, Failure> {
        if !dispatch.by_owner {
            return Ok(dispatch.target_platform_message_id);
        }
        let (chat, mode) = self.target(&dispatch.target_chat_id).await?;
        if is_status_broadcast(&chat) {
            return self.react_to_story(dispatch, &chat, mode).await;
        }
        let key = self.reaction_key(&dispatch, &chat, mode).await?;
        self.client.react(chat, key, dispatch.emoji).await?;
        Ok(dispatch.target_platform_message_id)
    }

    /// Revoking our own (bridge-authored) copy; the target id is returned
    /// unchanged for the acknowledgement.
    async fn delete_message(&self, dispatch: pb::MessageDeleteDispatch) -> Result<String, Failure> {
        let (chat, _) = self.target(&dispatch.target_chat_id).await?;
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
    use crate::addressing::Account;
    use crate::chat_state::MuteCall;
    use crate::config::Rendering;
    use crate::groups::GroupFacts;
    use crate::identity::LidStore;
    use crate::normalize::context_info;
    use crate::testing::FakeAccount;

    const DM: &str = "1555@s.whatsapp.net";
    const GROUP: &str = "123@g.us";

    /// WhatsApp, recorded: every send, upload, edit, revoke and chat action.
    #[derive(Default)]
    struct FakeClient {
        /// The paired account (own addresses, LID mappings, group modes).
        account: FakeAccount,
        sends: Mutex<Vec<(Jid, wa::Message)>>,
        uploads: Mutex<Vec<(Vec<u8>, OutboundMedia)>>,
        edits: Mutex<Vec<(Jid, String, wa::Message)>>,
        revokes: Mutex<Vec<(Jid, String)>>,
        reactions: Mutex<Vec<(Jid, wa::MessageKey, String)>>,
        actions: Mutex<Vec<ChatAction>>,
        /// The send (0-based) that fails, transiently.
        fail_send_at: Option<usize>,
        /// Runs once, after the first upload.
        after_upload: Mutex<Option<Box<dyn FnOnce() + Send>>>,
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

        async fn react(&self, to: Jid, key: wa::MessageKey, emoji: String) -> Result<(), Failure> {
            self.reactions.lock().unwrap().push((to, key, emoji));
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
            if let Some(hook) = self.after_upload.lock().unwrap().take() {
                hook();
            }
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
                OutboundMedia::Sticker(options) => wa::Message {
                    sticker_message: MessageField::some(wa::message::StickerMessage {
                        mimetype: options.mimetype,
                        is_animated: Some(options.is_animated),
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

        fn linked(&self) -> Result<(), Failure> {
            Ok(())
        }

        async fn group_facts(&self, group: &Jid) -> Option<GroupFacts> {
            self.groups.get(&group.to_string()).cloned()
        }
    }

    impl LidStore for FakeClient {
        async fn lid_for_pn(&self, pn: &Jid) -> Option<Jid> {
            self.account.lid_for_pn(pn).await
        }

        async fn pn_for_lid(&self, lid: &Jid) -> Option<Jid> {
            self.account.pn_for_lid(lid).await
        }
    }

    impl Account for FakeClient {
        fn own_pn(&self) -> Option<Jid> {
            self.account.own_pn()
        }

        fn own_lid(&self) -> Option<Jid> {
            self.account.own_lid()
        }

        async fn lid_migrated(&self) -> bool {
            self.account.lid_migrated().await
        }

        async fn group_mode(&self, group: &Jid) -> Option<AddressingMode> {
            self.account.group_mode(group).await
        }
    }

    /// A client paired as 1999.
    fn fake() -> FakeClient {
        FakeClient {
            account: FakeAccount {
                own_pn: Some("1999@s.whatsapp.net".parse().unwrap()),
                ..FakeAccount::default()
            },
            ..FakeClient::default()
        }
    }

    type TestSender = WhatsAppSender<FakeClient, FakeMedia, FakeConverter>;

    /// The media.convert provider: answers every conversion with `result`
    /// and records each call's media id, source mime, source name and target.
    struct FakeConverter {
        result: Result<Vec<u8>, String>,
        calls: Mutex<Vec<(String, String, String, String)>>,
    }

    impl Default for FakeConverter {
        fn default() -> Self {
            Self::answering(Err("no converter in this test".into()))
        }
    }

    impl FakeConverter {
        fn answering(result: Result<Vec<u8>, String>) -> Self {
            Self {
                result,
                calls: Mutex::default(),
            }
        }
    }

    impl StickerConverter for FakeConverter {
        async fn convert(
            &self,
            media_id: &str,
            source_mime: &str,
            source_name: &str,
            target_mime: &str,
        ) -> anyhow::Result<Vec<u8>> {
            self.calls.lock().unwrap().push((
                media_id.into(),
                source_mime.into(),
                source_name.into(),
                target_mime.into(),
            ));
            self.result.clone().map_err(anyhow::Error::msg)
        }
    }

    fn with(
        client: FakeClient,
        media: FakeMedia,
        runtime: Runtime,
        fallback: Option<TimerFallback>,
    ) -> TestSender {
        WhatsAppSender::new(
            client,
            media,
            FakeConverter::default(),
            Arc::new(Live::new(runtime)),
            fallback,
            Arc::default(),
            Arc::default(),
        )
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

    fn sender(client: FakeClient, media: FakeMedia) -> TestSender {
        with(client, media, Runtime::default(), None)
    }

    fn rendering(attachments_as_files: bool, max_message_length: u32) -> Runtime {
        Runtime {
            rendering: Rendering {
                attachments_as_files,
                max_message_length,
            },
            ..Runtime::default()
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
            // A copy the bridge sent here, as most replies quote.
            authored_by_bridge: true,
            ..Default::default()
        }
    }

    fn timer() -> pb::DisappearingTimer {
        pb::DisappearingTimer {
            seconds: 86_400,
            setting_timestamp: Some(1_700_000_000),
        }
    }

    fn sends(sender: &TestSender) -> Vec<wa::Message> {
        let sends = sender.client.sends.lock().unwrap();
        sends.iter().map(|(_, message)| message.clone()).collect()
    }

    fn context(message: &wa::Message) -> &wa::ContextInfo {
        context_info(message).expect("the message carries a context")
    }

    #[tokio::test]
    async fn text_is_sent_as_text() {
        let sender = sender(fake(), FakeMedia::default());
        let delivery = sender.send_message(text("hi")).await.unwrap();
        assert_eq!(delivery.message_id, "wa-1");
        assert_eq!(delivery.thread_id, None);
        assert_eq!(sends(&sender)[0].text_content(), Some("hi"));
    }

    #[tokio::test]
    async fn a_reply_quotes_its_parent() {
        let sender = sender(fake(), FakeMedia::default());
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
    async fn a_quote_names_who_wrote_the_parent() {
        let sender = sender(fake(), FakeMedia::default());
        let received = |participant: Option<&str>, sender_id: &str| pb::ReplyContext {
            authored_by_bridge: false,
            participant: participant.map(str::to_owned),
            sender: sender_id.into(),
            ..reply_to("wa-0")
        };
        for (reply, author) in [
            // A group message: its participant.
            (
                received(Some("1777:3@s.whatsapp.net"), "123@g.us"),
                "1777@s.whatsapp.net",
            ),
            // A DM: its sender.
            (received(None, "1888@lid"), "1888@lid"),
            // Nothing usable: our own account, as for a bridge-authored copy.
            (received(None, ""), "1999@s.whatsapp.net"),
            (
                pb::ReplyContext {
                    participant: Some("1777@s.whatsapp.net".into()),
                    ..reply_to("wa-0")
                },
                "1999@s.whatsapp.net",
            ),
        ] {
            let dispatch = pb::MessageCreateDispatch {
                target_chat_id: GROUP.into(),
                reply_context: Some(reply),
                ..text("here")
            };
            sender.send_message(dispatch).await.unwrap();
            let sent = sends(&sender);
            let quote = context(sent.last().unwrap());
            assert_eq!(quote.participant.as_deref(), Some(author));
        }
    }

    #[tokio::test]
    async fn a_reply_id_without_a_context_is_permanent() {
        let sender = sender(fake(), FakeMedia::default());
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
        let sender = sender(fake(), media(&["m-1"]));
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
        let sender = sender(fake(), FakeMedia::default());
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
        let sender = sender(fake(), FakeMedia::default());
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
            ..fake()
        };
        let (inputs, mut frames) = mpsc::channel(4);
        let sender = with(
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
        let sender = sender(fake(), media(&["m-1"]));
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
        let sender = with(fake(), media(&["m-1"]), rendering(true, 0), None);
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
        let sender = with(fake(), FakeMedia::default(), rendering(false, 5), None);
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
        let sender = sender(fake(), media(&["m-1"]));
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
            fake(),
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
        let sender = sender(fake(), media(&["m-1", "m-2", "m-3"]));
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
            ..fake()
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
            ..fake()
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
        let sender = with(fake(), media(&["m"]), rendering(false, 3), None);
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

    /// A client paired as 1999 (LID 7) that knows 1555 is 9, with a
    /// LID-addressed group 123 and a phone-addressed group 456.
    fn addressed() -> FakeClient {
        FakeClient {
            account: FakeAccount {
                own_pn: Some("1999@s.whatsapp.net".parse().unwrap()),
                own_lid: Some("7@lid".parse().unwrap()),
                group_modes: HashMap::from([
                    ("123@g.us".to_owned(), AddressingMode::Lid),
                    ("456@g.us".to_owned(), AddressingMode::Pn),
                ]),
                ..FakeAccount::mapping("1555", "9")
            },
            ..FakeClient::default()
        }
    }

    async fn quoted_participant(
        sender: &TestSender,
        chat: &str,
        reply: pb::ReplyContext,
    ) -> Option<String> {
        let dispatch = pb::MessageCreateDispatch {
            target_chat_id: chat.into(),
            reply_context: Some(reply),
            ..text("here")
        };
        sender.send_message(dispatch).await.unwrap();
        let sent = sends(sender);
        context(sent.last().unwrap()).participant.clone()
    }

    #[tokio::test]
    async fn a_received_copy_is_quoted_under_its_author_in_the_chat_s_form() {
        let sender = sender(addressed(), FakeMedia::default());
        let received = |participant: &str| pb::ReplyContext {
            authored_by_bridge: false,
            participant: Some(participant.into()),
            ..reply_to("wa-0")
        };
        // Canonical ids name the author by LID; a phone-addressed group quotes
        // the phone form, a LID-addressed one the LID.
        assert_eq!(
            quoted_participant(&sender, "456@g.us", received("9@lid"))
                .await
                .as_deref(),
            Some("1555@s.whatsapp.net")
        );
        assert_eq!(
            quoted_participant(&sender, "123@g.us", received("1555@s.whatsapp.net"))
                .await
                .as_deref(),
            Some("9@lid")
        );
    }

    #[tokio::test]
    async fn a_bridge_copy_is_quoted_under_our_own_address_in_the_chat_s_form() {
        let sender = sender(addressed(), FakeMedia::default());
        assert_eq!(
            quoted_participant(&sender, "123@g.us", reply_to("wa-0"))
                .await
                .as_deref(),
            Some("7@lid")
        );
        assert_eq!(
            quoted_participant(&sender, "456@g.us", reply_to("wa-0"))
                .await
                .as_deref(),
            Some("1999@s.whatsapp.net")
        );
    }

    /// The owner's reaction on `id` in `chat`, the reacted copy written by
    /// `participant` (received) or by the bridge (`None`).
    fn owner_reaction(chat: &str, participant: Option<&str>, emoji: &str) -> pb::ReactionDispatch {
        pb::ReactionDispatch {
            target_chat_id: chat.into(),
            target_platform_message_id: "wa-7".into(),
            emoji: emoji.into(),
            by_owner: true,
            target_sender: chat.into(),
            target_participant: participant.map(str::to_owned),
            target_authored_by_bridge: participant.is_none(),
            ..pb::ReactionDispatch::default()
        }
    }

    fn reactions(sender: &TestSender) -> Vec<(Jid, wa::MessageKey, String)> {
        sender.client.reactions.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn the_owners_reaction_names_a_received_group_message_by_its_author() {
        let sender = sender(addressed(), FakeMedia::default());
        let id = sender
            .apply_reaction(owner_reaction(
                "123@g.us",
                Some("1555@s.whatsapp.net"),
                "❤️",
            ))
            .await
            .unwrap();
        assert_eq!(id, "wa-7");
        let [(chat, key, emoji)] = reactions(&sender).try_into().unwrap();
        assert_eq!(chat.to_string(), "123@g.us");
        assert_eq!(emoji, "❤️");
        // The LID-addressed group knows the author by its LID.
        assert_eq!(
            key,
            wa::MessageKey {
                remote_jid: Some("123@g.us".into()),
                from_me: Some(false),
                id: Some("wa-7".into()),
                participant: Some("9@lid".into()),
            }
        );
    }

    #[tokio::test]
    async fn the_owners_reaction_on_a_bridge_copy_is_from_me() {
        let sender = sender(addressed(), FakeMedia::default());
        sender
            .apply_reaction(owner_reaction("456@g.us", None, "👍"))
            .await
            .unwrap();
        let [(_, key, _)] = reactions(&sender).try_into().unwrap();
        assert_eq!(key.from_me, Some(true));
        assert_eq!(key.participant, None);
    }

    #[tokio::test]
    async fn the_owners_reaction_in_a_dm_names_no_participant() {
        let sender = sender(fake(), FakeMedia::default());
        sender
            .apply_reaction(owner_reaction(DM, Some(DM), ""))
            .await
            .unwrap();
        let [(chat, key, emoji)] = reactions(&sender).try_into().unwrap();
        assert_eq!(chat.to_string(), DM);
        assert_eq!(emoji, "", "an empty emoji removes the reaction");
        assert_eq!(key.from_me, Some(false));
        assert_eq!(key.participant, None);
    }

    #[tokio::test]
    async fn someone_elses_reaction_sends_nothing() {
        let sender = sender(fake(), FakeMedia::default());
        let dispatch = pb::ReactionDispatch {
            by_owner: false,
            ..owner_reaction(GROUP, Some(DM), "😂")
        };
        assert_eq!(sender.apply_reaction(dispatch).await.unwrap(), "wa-7");
        assert!(reactions(&sender).is_empty());
    }

    #[tokio::test]
    async fn an_unpaired_account_cannot_quote() {
        let sender = sender(FakeClient::default(), FakeMedia::default());
        let dispatch = pb::MessageCreateDispatch {
            reply_context: Some(reply_to("wa-0")),
            ..text("here")
        };
        let Err(failure) = sender.send_message(dispatch).await else {
            panic!("nothing to quote as");
        };
        assert!(failure.is_permanent());
    }

    #[tokio::test]
    async fn a_chat_is_addressed_in_its_form() {
        let addresses = Arc::new(AddressBook::default());
        // The DM last arrived under its phone form.
        addresses.observe(
            "9@lid",
            &whatsapp_rust::types::message::MessageSource {
                chat: "1555@s.whatsapp.net".parse().unwrap(),
                ..Default::default()
            },
        );
        let sender = WhatsAppSender::new(
            addressed(),
            FakeMedia::default(),
            FakeConverter::default(),
            Arc::new(Live::new(Runtime::default())),
            None,
            Arc::default(),
            addresses,
        );
        let dispatch = pb::MessageCreateDispatch {
            target_chat_id: "9@lid".into(),
            ..text("hi")
        };
        sender.send_message(dispatch).await.unwrap();
        let to = sender.client.sends.lock().unwrap()[0].0.clone();
        assert_eq!(to.to_string(), "1555@s.whatsapp.net");
    }

    /// A static WebP, as the converter returns one and Telegram sends a static
    /// sticker.
    const WEBP_STICKER: &[u8] =
        include_bytes!("../../whatsapp/internal/whatsapp/testdata/sticker-lossy.webp");

    fn sticker(mime: &str, animated: bool) -> pb::Attachment {
        pb::Attachment {
            media_id: "s-1".into(),
            kind: pb::attachment::Kind::Sticker.into(),
            mime_type: Some(mime.into()),
            animated,
            ..Default::default()
        }
    }

    /// A sender whose core holds `stored` as the sticker's bytes.
    fn sticker_sender(converter: FakeConverter, runtime: Runtime, stored: &[u8]) -> TestSender {
        let media = FakeMedia {
            blobs: HashMap::from([("s-1".to_owned(), stored.to_vec())]),
            fail: false,
        };
        WhatsAppSender::new(
            fake(),
            media,
            converter,
            Arc::new(Live::new(runtime)),
            None,
            Arc::default(),
            Arc::default(),
        )
    }

    async fn send_sticker(sender: &TestSender, attachment: pb::Attachment) {
        let dispatch = pb::MessageCreateDispatch {
            attachments: vec![attachment],
            ..text("")
        };
        sender.send_message(dispatch).await.unwrap();
    }

    fn only_upload(sender: &TestSender) -> (Vec<u8>, OutboundMedia) {
        let uploads = sender.client.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        uploads[0].clone()
    }

    fn sticker_options(media: &OutboundMedia) -> Option<&StickerOptions> {
        match media {
            OutboundMedia::Sticker(options) => Some(options),
            _ => None,
        }
    }

    fn carries(bytes: &[u8], needle: &str) -> bool {
        bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    #[tokio::test]
    async fn an_animated_webm_sticker_is_converted() {
        let converter = FakeConverter::answering(Ok(WEBP_STICKER.to_vec()));
        let sender = sticker_sender(converter, Runtime::default(), b"webm");
        let attachment = pb::Attachment {
            file_name: Some("hi.webm".into()),
            ..sticker("video/webm", true)
        };
        send_sticker(&sender, attachment).await;

        assert_eq!(
            *sender.converter.calls.lock().unwrap(),
            [(
                "s-1".into(),
                "video/webm".into(),
                "hi.webm".into(),
                "image/webp".into()
            )]
        );
        let (bytes, media) = only_upload(&sender);
        assert_eq!(
            sticker_options(&media),
            Some(&StickerOptions {
                mimetype: Some("image/webp".into()),
                is_animated: true,
            })
        );
        assert!(carries(&bytes, r#""sticker-pack-name":"wuphf""#));
        assert!(sends(&sender)[0].sticker_message.is_set());
    }

    #[tokio::test]
    async fn a_lottie_sticker_is_converted() {
        let converter = FakeConverter::answering(Ok(WEBP_STICKER.to_vec()));
        let sender = sticker_sender(converter, Runtime::default(), b"tgs");
        send_sticker(&sender, sticker("application/x-tgsticker", true)).await;
        let calls = sender.converter.calls.lock().unwrap().clone();
        assert_eq!(calls[0].1, "application/x-tgsticker");
        let (bytes, media) = only_upload(&sender);
        assert!(matches!(media, OutboundMedia::Sticker(_)));
        assert!(carries(&bytes, r#""sticker-pack-name":"wuphf""#));
    }

    #[tokio::test]
    async fn a_failed_conversion_sends_the_original_as_a_file() {
        let converter = FakeConverter::answering(Err("no provider".into()));
        let sender = sticker_sender(converter, Runtime::default(), b"webm");
        let dispatch = pb::MessageCreateDispatch {
            attachments: vec![sticker("video/webm", true)],
            ..text("a caption")
        };
        sender.send_message(dispatch).await.unwrap();
        let (bytes, media) = only_upload(&sender);
        assert_eq!(bytes, b"webm");
        let OutboundMedia::Document(document) = media else {
            panic!("a document upload");
        };
        assert_eq!(document.file_name.as_deref(), Some("sticker.webm"));
        assert_eq!(document.caption, None);
    }

    #[tokio::test]
    async fn a_static_webp_sticker_is_not_converted() {
        let sender = sticker_sender(FakeConverter::default(), Runtime::default(), WEBP_STICKER);
        send_sticker(&sender, sticker("image/webp", false)).await;
        assert!(sender.converter.calls.lock().unwrap().is_empty());
        let (bytes, media) = only_upload(&sender);
        assert_eq!(
            sticker_options(&media),
            Some(&StickerOptions {
                mimetype: Some("image/webp".into()),
                is_animated: false,
            })
        );
        assert!(carries(&bytes, r#""sticker-pack-name":"wuphf""#));
    }

    #[tokio::test]
    async fn the_configured_pack_metadata_is_used() {
        let runtime = Runtime {
            sticker_metadata: crate::config::StickerMetadata {
                pack_name: "mine".into(),
                ..Default::default()
            },
            ..Runtime::default()
        };
        let sender = sticker_sender(FakeConverter::default(), runtime, WEBP_STICKER);
        send_sticker(&sender, sticker("image/webp", false)).await;
        let (bytes, _) = only_upload(&sender);
        assert!(carries(&bytes, r#""sticker-pack-name":"mine""#));
        assert!(carries(&bytes, r#""sticker-pack-publisher":"wuphf-ai"#));
    }

    #[tokio::test]
    async fn a_sticker_carries_the_timer_and_the_reply() {
        let converter = FakeConverter::answering(Ok(WEBP_STICKER.to_vec()));
        let sender = sticker_sender(converter, Runtime::default(), b"webm");
        let dispatch = pb::MessageCreateDispatch {
            reply_context: Some(reply_to("wa-0")),
            attachments: vec![sticker("video/webm", true)],
            disappearing: Some(timer()),
            ..text("")
        };
        sender.send_message(dispatch).await.unwrap();
        let message = &sends(&sender)[0];
        assert!(message.sticker_message.is_set());
        let context = context(message);
        assert_eq!(context.stanza_id.as_deref(), Some("wa-0"));
        assert_eq!(context.expiration, Some(86_400));
    }

    #[tokio::test]
    async fn files_mode_sends_stickers_as_files() {
        let sender = sticker_sender(FakeConverter::default(), rendering(true, 0), b"webm");
        send_sticker(&sender, sticker("video/webm", true)).await;
        assert!(sender.converter.calls.lock().unwrap().is_empty());
        let (bytes, media) = only_upload(&sender);
        assert_eq!(bytes, b"webm");
        assert!(matches!(media, OutboundMedia::Document(_)));
    }

    #[tokio::test]
    async fn a_changed_setting_applies_to_the_next_dispatch() {
        let sender = sticker_sender(FakeConverter::default(), Runtime::default(), WEBP_STICKER);
        sender.send_message(text("ten chars!")).await.unwrap();
        sender.runtime.set(Runtime {
            sticker_metadata: crate::config::StickerMetadata {
                pack_name: "mine".into(),
                ..Default::default()
            },
            ..rendering(false, 5)
        });
        sender.send_message(text("ten chars!")).await.unwrap();
        send_sticker(&sender, sticker("image/webp", false)).await;

        let sent = sends(&sender);
        assert_eq!(sent[0].text_content(), Some("ten chars!"));
        assert_eq!(
            sent[1].document_message.file_name.as_deref(),
            Some("message.txt")
        );
        let uploads = sender.client.uploads.lock().unwrap();
        assert!(carries(&uploads[1].0, r#""sticker-pack-name":"mine""#));
    }

    #[tokio::test]
    async fn one_dispatch_sees_one_value() {
        let sender = sender(fake(), media(&["m-1", "m-2", "m-3"]));
        let live = Arc::clone(&sender.runtime);
        *sender.client.after_upload.lock().unwrap() = Some(Box::new(move || {
            live.set(rendering(true, 0));
        }));
        let dispatch = pb::MessageCreateDispatch {
            attachments: vec![image("m-1"), image("m-2"), image("m-3")],
            ..text("three")
        };
        sender.send_message(dispatch).await.unwrap();
        let uploads = sender.client.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 3);
        for (_, media) in uploads.iter() {
            assert!(matches!(media, OutboundMedia::Image(_)), "{media:?}");
        }
        // The change applies from the next dispatch.
        assert!(sender.runtime.load().rendering.attachments_as_files);
    }

    // ---- Plan 083: status@broadcast ----

    const STATUS: &str = "status@broadcast";
    const POSTER: &str = "1555@s.whatsapp.net";

    /// A send or an edit to status@broadcast would post a story to every
    /// contact: refused as permanent, nothing sent.
    #[tokio::test]
    async fn sends_and_edits_to_status_broadcast_are_refused() {
        let sender = sender(fake(), FakeMedia::default());
        let send = pb::MessageCreateDispatch {
            target_chat_id: STATUS.into(),
            ..text("lol")
        };
        assert!(matches!(
            sender.send_message(send).await,
            Err(Failure::Permanent(_))
        ));
        let edit = pb::MessageEditDispatch {
            target_chat_id: STATUS.into(),
            target_platform_message_id: "s-1".into(),
            text: Some("lol".into()),
            ..Default::default()
        };
        assert!(matches!(
            sender.edit_message(edit).await,
            Err(Failure::Permanent(_))
        ));
        assert!(sender.client.sends.lock().unwrap().is_empty());
        assert!(sender.client.edits.lock().unwrap().is_empty());
    }

    /// Revoking a story (the owner's own) still goes out.
    #[tokio::test]
    async fn a_delete_on_status_broadcast_revokes() {
        let sender = sender(fake(), FakeMedia::default());
        let delete = pb::MessageDeleteDispatch {
            target_chat_id: STATUS.into(),
            target_platform_message_id: "s-1".into(),
            ..Default::default()
        };
        sender.delete_message(delete).await.unwrap();
        let revokes = sender.client.revokes.lock().unwrap();
        assert_eq!(revokes[0].0.to_string(), STATUS);
        assert_eq!(revokes[0].1, "s-1");
    }

    fn story_reaction(emoji: &str) -> pb::ReactionDispatch {
        pb::ReactionDispatch {
            target_chat_id: STATUS.into(),
            target_platform_message_id: "story-1".into(),
            target_sender: POSTER.into(),
            target_participant: Some(POSTER.into()),
            by_owner: true,
            emoji: emoji.into(),
            ..Default::default()
        }
    }

    /// The owner's reaction on a story goes to the poster's DM as an emoji
    /// quoting the story, never to status@broadcast.
    #[tokio::test]
    async fn a_story_reaction_is_an_emoji_in_the_posters_dm() {
        let sender = sender(fake(), FakeMedia::default());
        let id = sender.apply_reaction(story_reaction("❤️")).await.unwrap();
        assert_eq!(id, "story-1");
        assert!(sender.client.reactions.lock().unwrap().is_empty());
        let sends = sender.client.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        let (to, message) = &sends[0];
        assert_eq!(to.to_string(), POSTER);
        assert_eq!(message.text_content(), Some("❤️"));
        let quote = context(message);
        assert_eq!(quote.stanza_id.as_deref(), Some("story-1"));
        assert_eq!(quote.participant.as_deref(), Some(POSTER));
        assert_eq!(quote.remote_jid.as_deref(), Some(STATUS));
    }

    /// Clearing a story reaction, or reacting to the owner's own story,
    /// sends nothing and is acknowledged.
    #[tokio::test]
    async fn story_reactions_that_send_nothing() {
        let own = pb::ReactionDispatch {
            target_authored_by_bridge: true,
            ..story_reaction("❤️")
        };
        for dispatch in [story_reaction(""), own] {
            let sender = sender(fake(), FakeMedia::default());
            assert_eq!(sender.apply_reaction(dispatch).await.unwrap(), "story-1");
            assert!(sender.client.sends.lock().unwrap().is_empty());
            assert!(sender.client.reactions.lock().unwrap().is_empty());
        }
    }

    /// A reply quoting Bob's story in Bob's DM names status@broadcast as the
    /// quoted chat, so WhatsApp shows a status reply.
    #[tokio::test]
    async fn a_quote_of_another_chat_names_it() {
        let sender = sender(fake(), FakeMedia::default());
        let dispatch = pb::MessageCreateDispatch {
            target_chat_id: POSTER.into(),
            reply_context: Some(pb::ReplyContext {
                platform_message_id: "story-1".into(),
                sender: POSTER.into(),
                participant: Some(POSTER.into()),
                chat_id: Some(STATUS.into()),
                ..Default::default()
            }),
            ..text("nice")
        };
        sender.send_message(dispatch).await.unwrap();
        let sends = sender.client.sends.lock().unwrap();
        let (to, message) = &sends[0];
        assert_eq!(to.to_string(), POSTER);
        let quote = context(message);
        assert_eq!(quote.remote_jid.as_deref(), Some(STATUS));
        assert_eq!(quote.stanza_id.as_deref(), Some("story-1"));
        assert_eq!(quote.participant.as_deref(), Some(POSTER));
    }
}
